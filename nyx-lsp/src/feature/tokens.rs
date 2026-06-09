//! Semantic-tokens legend and delta encoder
//!
//! Maps [`super::highlight`] output onto the LSP wire format: a flat `u32` array
//! of `[Δline, Δstart, length, typeIndex, modifierBits]`, position-sorted and
//! delta-encoded, in the client's negotiated encoding

use super::highlight::{HighlightToken, TokenModifiers, TokenType};
use crate::convert::Encoding;
use tower_lsp::lsp_types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend,
};

/// The legend the server advertises, token indices below must match its order
#[inline]
pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::NAMESPACE,
            SemanticTokenType::TYPE,
            SemanticTokenType::FUNCTION,
            SemanticTokenType::METHOD,
            SemanticTokenType::PARAMETER,
            SemanticTokenType::VARIABLE,
            SemanticTokenType::PROPERTY,
            SemanticTokenType::KEYWORD,
            SemanticTokenType::COMMENT,
            SemanticTokenType::STRING,
            SemanticTokenType::NUMBER,
            SemanticTokenType::OPERATOR,
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,
            SemanticTokenModifier::READONLY,
            SemanticTokenModifier::new("mutable"),
        ],
    }
}

pub fn encode(tokens: &[HighlightToken], encoding: Encoding) -> Vec<SemanticToken> {
    let utf16 = matches!(encoding, Encoding::Utf16);
    let mut data = Vec::with_capacity(tokens.len());
    let (mut prev_line, mut prev_start) = (0, 0);

    for token in tokens {
        let (start, length) = match utf16 {
            true => (token.start_utf16, token.len_utf16),
            false => (token.start_utf8, token.len_utf8),
        };
        let delta_line = token.line - prev_line;
        let delta_start = match delta_line {
            0 => start - prev_start,
            _ => start,
        };

        data.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: type_index(token.ty),
            token_modifiers_bitset: modifier_bits(token.modifiers),
        });

        prev_line = token.line;
        prev_start = start;
    }

    data
}

#[inline]
const fn type_index(ty: TokenType) -> u32 {
    match ty {
        TokenType::Namespace => 0,
        TokenType::Type => 1,
        TokenType::Function => 2,
        TokenType::Method => 3,
        TokenType::Parameter => 4,
        TokenType::Variable => 5,
        TokenType::Property => 6,
        TokenType::Keyword => 7,
        TokenType::Comment => 8,
        TokenType::String => 9,
        TokenType::Number | TokenType::Boolean => 10,
        TokenType::Operator => 11,
    }
}

#[inline]
const fn modifier_bits(modifiers: TokenModifiers) -> u32 {
    (modifiers.declaration as u32)
        | (modifiers.readonly as u32) << 1
        | (modifiers.mutable as u32) << 2
}
