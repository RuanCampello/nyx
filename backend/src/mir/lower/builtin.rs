use crate::mir::{
    Const, InstructionKind as Kind, Operand, OverflowMode, Place, StringPool, Terminator,
    lower::{FunctionBuildState, LoweringContext},
};
use frontend::{
    hir::{self, Type, lang::PrintKind},
    parser::expression::BinaryOperator,
};

pub(in crate::mir::lower) struct BuiltinLowering<'b, 'a, 'hir> {
    context: LoweringContext<'a, 'hir>,
    build: &'b mut FunctionBuildState<'hir>,
    strings: &'b mut StringPool,
}

impl<'b, 'a, 'hir> BuiltinLowering<'b, 'a, 'hir> {
    pub(super) fn new(
        context: LoweringContext<'a, 'hir>,
        build: &'b mut FunctionBuildState<'hir>,
        strings: &'b mut StringPool,
    ) -> Self {
        Self { context, build, strings }
    }

    pub(super) fn flush_text(&mut self, pending: &mut String) {
        if !pending.is_empty() {
            self.emit_write_string(std::mem::take(pending));
        }
    }

    pub(super) fn emit_value(&mut self, kind: PrintKind, operand: Operand<'hir>) {
        match kind {
            PrintKind::Str => self.emit_write_str(operand),
            PrintKind::Bool => self.emit_write_bool(operand),
            PrintKind::Char => self.emit_write_char(operand),
            PrintKind::Int => self.emit_write_digits(operand, true),
            PrintKind::Uint => self.emit_write_digits(operand, false),
        }
    }

    fn emit_write_str(&mut self, operand: Operand<'hir>) {
        let uptr = self.context.types.common.uptr;
        let pointer = self.build.fresh_temporary(uptr);
        self.build.emit(pointer, Kind::FieldLoad { src: operand, offset: 0, typ: uptr });

        let len = self.build.fresh_temporary(uptr);
        self.build.emit(len, Kind::FieldLoad { src: operand, offset: 8, typ: uptr });

        self.emit_write(Operand::Place(pointer), Operand::Place(len));
    }

    fn emit_write_bool(&mut self, condition: Operand<'hir>) {
        let (then_id, else_id, merge_id) =
            (self.build.new_block(), self.build.new_block(), self.build.new_block());

        self.build.terminate(Terminator::Branch {
            condition,
            then_block: then_id,
            else_block: else_id,
        });

        self.build.switch_to(then_id);
        self.emit_write_string("true".to_owned());
        self.build.terminate(Terminator::Jump(merge_id));

        self.build.switch_to(else_id);
        self.emit_write_string("false".to_owned());
        self.build.terminate(Terminator::Jump(merge_id));

        self.build.switch_to(merge_id);
    }

    fn emit_write_char(&mut self, point: Operand<'hir>) {
        let u32 = self.context.types.common.u32;
        let buffer = self.byte_buffer(4);

        let point = self.cast(point, u32);
        let one = self.compare(BinaryOperator::Lt, point, self.int(128, u32));
        let two = self.compare(BinaryOperator::Lt, point, self.int(2048, u32));
        let three = self.compare(BinaryOperator::Lt, point, self.int(65536, u32));
        let conditions = [one, two, three];

        let low = self.trailing_byte(point, 1, u32);
        let mid = self.trailing_byte(point, 64, u32);
        let high = self.trailing_byte(point, 4096, u32);

        let lead_two = self.lead_byte(point, 64, 192, u32);
        let lead_three = self.lead_byte(point, 4096, 224, u32);
        let lead_four = self.lead_byte(point, 262144, 240, u32);

        let byte0 = self.select(conditions, [point, lead_two, lead_three, lead_four], u32);
        let byte1 = self.select(conditions, [low, low, mid, high], u32);
        let byte2 = self.select(conditions, [low, low, low, mid], u32);
        let byte3 = self.select(conditions, [low, low, low, low], u32);

        for (index, byte) in [byte0, byte1, byte2, byte3].into_iter().enumerate() {
            self.store_byte(buffer, index as i64, byte, 4);
        }

        let len = self.select(
            conditions,
            [self.int(1, u32), self.int(2, u32), self.int(3, u32), self.int(4, u32)],
            u32,
        );

        let pointer = self.element_address(buffer, self.int(0, self.context.types.common.uptr), 4);
        self.emit_write(pointer, len);
    }

    fn emit_write_digits(&mut self, value: Operand<'hir>, signed: bool) {
        const WIDTH: u32 = 24;
        let (uptr, typ) = (self.context.types.common.uptr, self.context.types.common.u64);
        let buffer = self.byte_buffer(WIDTH);

        let index = self.build.fresh_temporary(uptr);
        self.build.emit(index, Kind::Assign(self.int(WIDTH as i64, uptr)));

        let rest = self.build.fresh_temporary(typ);
        let negative = match signed {
            true => {
                let signed_typ = self.context.types.common.i64;
                let value = self.cast(value, signed_typ);
                let zero = self.int(0, signed_typ);
                let negative = self.compare(BinaryOperator::Lt, value, zero);

                let raw = self.cast(value, typ);
                let flipped = self.binary(BinaryOperator::Sub, self.int(0, typ), raw, typ);
                let magnitude = self.build.fresh_temporary(typ);

                self.build.emit(
                    magnitude,
                    Kind::Select { condition: negative, then_value: flipped, else_value: raw },
                );
                self.build.emit(rest, Kind::Assign(Operand::Place(magnitude)));

                Some(negative)
            },
            _ => {
                let value = self.cast(value, typ);
                self.build.emit(rest, Kind::Assign(value));
                None
            },
        };

        let (body_id, done_id) = (self.build.new_block(), self.build.new_block());
        self.build.terminate(Terminator::Jump(body_id));
        self.build.switch_to(body_id);

        let next = self.binary(BinaryOperator::Sub, Operand::Place(index), self.int(1, uptr), uptr);
        self.build.emit(index, Kind::Assign(next));

        let quotient =
            self.binary(BinaryOperator::Div, Operand::Place(rest), self.int(10, typ), typ);
        let scaled = self.binary(BinaryOperator::Mul, quotient, self.int(10, typ), typ);
        let digit = self.binary(BinaryOperator::Sub, Operand::Place(rest), scaled, typ);
        let character = self.binary(BinaryOperator::Add, digit, self.int(48, typ), typ);

        self.store_byte_at(buffer, Operand::Place(index), character, WIDTH);
        self.build.emit(rest, Kind::Assign(quotient));

        let finished = self.compare(BinaryOperator::Eq, Operand::Place(rest), self.int(0, typ));
        self.build.terminate(Terminator::Branch {
            condition: finished,
            then_block: done_id,
            else_block: body_id,
        });

        self.build.switch_to(done_id);

        if let Some(negative) = negative {
            let (sign_id, write_id) = (self.build.new_block(), self.build.new_block());
            self.build.terminate(Terminator::Branch {
                condition: negative,
                then_block: sign_id,
                else_block: write_id,
            });

            self.build.switch_to(sign_id);
            let before =
                self.binary(BinaryOperator::Sub, Operand::Place(index), self.int(1, uptr), uptr);
            self.build.emit(index, Kind::Assign(before));
            self.store_byte_at(buffer, Operand::Place(index), self.int(45, typ), WIDTH);
            self.build.terminate(Terminator::Jump(write_id));

            self.build.switch_to(write_id);
        }

        let pointer = self.element_address(buffer, Operand::Place(index), WIDTH);
        let len = self.binary(
            BinaryOperator::Sub,
            self.int(WIDTH as i64, uptr),
            Operand::Place(index),
            uptr,
        );

        self.emit_write(pointer, len);
    }

    fn byte_buffer(&mut self, len: u32) -> Place<'hir> {
        let id = self.context.arrays.intern(self.context.types.common.u8, len);
        self.build.fresh_temporary(self.context.types.array(id))
    }

    fn emit_write(&mut self, pointer: Operand<'hir>, len: Operand<'hir>) {
        let i32 = self.context.types.common.i32;
        let dest = self.build.fresh_temporary(i32);

        self.build.emit(
            dest,
            Kind::Syscall {
                code: hir::Syscall::Write,
                args: vec![Operand::Const(Const::Int(1, i32)), pointer, len],
                returns: false,
            },
        );
    }

    #[inline(always)]
    fn int(&self, value: i64, typ: Type<'hir>) -> Operand<'hir> {
        Operand::Const(Const::Int(value, typ))
    }

    fn binary(
        &mut self,
        operation: BinaryOperator,
        lhs: Operand<'hir>,
        rhs: Operand<'hir>,
        typ: Type<'hir>,
    ) -> Operand<'hir> {
        let dest = self.build.fresh_temporary(typ);
        let instr = Kind::Binary { operation, lhs, rhs, overflow: OverflowMode::Wrapping };
        self.build.emit(dest, instr);
        Operand::Place(dest)
    }

    fn compare(
        &mut self,
        operation: BinaryOperator,
        lhs: Operand<'hir>,
        rhs: Operand<'hir>,
    ) -> Operand<'hir> {
        let dest = self.build.fresh_temporary(self.context.types.common.bool);
        let instr = Kind::Binary { operation, lhs, rhs, overflow: OverflowMode::Unchecked };
        self.build.emit(dest, instr);
        Operand::Place(dest)
    }

    fn cast(&mut self, src: Operand<'hir>, typ: Type<'hir>) -> Operand<'hir> {
        let dest = self.build.fresh_temporary(typ);
        self.build.emit(dest, Kind::Cast { src, typ });
        Operand::Place(dest)
    }

    fn trailing_byte(
        &mut self,
        point: Operand<'hir>,
        shift: i64,
        typ: Type<'hir>,
    ) -> Operand<'hir> {
        let shifted = self.binary(BinaryOperator::Div, point, self.int(shift, typ), typ);
        let folded = self.binary(BinaryOperator::Div, shifted, self.int(64, typ), typ);
        let scaled = self.binary(BinaryOperator::Mul, folded, self.int(64, typ), typ);
        let low = self.binary(BinaryOperator::Sub, shifted, scaled, typ);
        self.binary(BinaryOperator::Add, low, self.int(128, typ), typ)
    }

    fn lead_byte(
        &mut self,
        point: Operand<'hir>,
        shift: i64,
        marker: i64,
        typ: Type<'hir>,
    ) -> Operand<'hir> {
        let shifted = self.binary(BinaryOperator::Div, point, self.int(shift, typ), typ);
        self.binary(BinaryOperator::Add, shifted, self.int(marker, typ), typ)
    }

    fn select(
        &mut self,
        [one, two, three]: [Operand<'hir>; 3],
        [a, b, c, d]: [Operand<'hir>; 4],
        typ: Type<'hir>,
    ) -> Operand<'hir> {
        let inner = self.build.fresh_temporary(typ);
        let instr = Kind::Select { condition: three, then_value: c, else_value: d };
        self.build.emit(inner, instr);

        let middle = self.build.fresh_temporary(typ);
        let else_value = Operand::Place(inner);
        let instr = Kind::Select { condition: two, then_value: b, else_value };
        self.build.emit(middle, instr);

        let outer = self.build.fresh_temporary(typ);
        let else_value = Operand::Place(middle);
        let instr = Kind::Select { condition: one, then_value: a, else_value };
        self.build.emit(outer, instr);

        Operand::Place(outer)
    }

    fn store_byte(&mut self, buffer: Place<'hir>, index: i64, value: Operand<'hir>, bound: u32) {
        let uptr = self.context.types.common.uptr;
        self.store_byte_at(buffer, self.int(index, uptr), value, bound);
    }

    fn store_byte_at(
        &mut self,
        buffer: Place<'hir>,
        index: Operand<'hir>,
        value: Operand<'hir>,
        bound: u32,
    ) {
        let uptr = self.context.types.common.uptr;
        let byte = self.cast(value, self.context.types.common.u8);
        let bound = self.int(bound as i64, uptr);
        let instr = Kind::ElementStore { index, bound, value: byte, stride: 1 };
        self.build.emit(buffer, instr);
    }

    fn element_address(
        &mut self,
        buffer: Place<'hir>,
        index: Operand<'hir>,
        bound: u32,
    ) -> Operand<'hir> {
        let uptr = self.context.types.common.uptr;
        let dest = self.build.fresh_temporary(uptr);
        let bound = self.int(bound as i64, uptr);

        let instr = Kind::ElementAddr { base: Operand::Place(buffer), index, bound, stride: 1 };
        self.build.emit(dest, instr);
        Operand::Place(dest)
    }

    fn emit_write_string(&mut self, text: String) {
        let len = text.len();
        let (id, i32) = (self.strings.intern(text), self.context.types.common.i32);
        let dest = self.build.fresh_temporary(i32);

        self.build.emit(
            dest,
            Kind::Syscall {
                code: hir::Syscall::Write,
                args: vec![
                    Operand::Const(Const::Int(1, i32)),
                    Operand::Const(Const::Str(id)),
                    Operand::Const(Const::Int(len as i64, i32)),
                ],
                returns: false,
            },
        );
    }
}
