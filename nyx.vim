" autocmd BufRead,BufNewFile *.nyx set filetype=nyx

if exists("b:current_syntax")
  finish
endif

" Keywords
syn keyword nyxKeyword fn let mut if else return while for struct inline const pub use impl
hi def link nyxKeyword Keyword

" Built-in Types
syn keyword nyxType i8 u8 i16 u16 i32 i64 f32 f64 bool char String uptr iptr
hi def link nyxType Type

" Custom Types
syn match nyxStruct "\<[A-Z][a-zA-Z0-9_]*\>"
hi def link nyxStruct Type

" Booleans
syn keyword nyxBoolean true false
hi def link nyxBoolean Boolean

" Numbers
syn match nyxNumber "\<\d\+\>"
hi def link nyxNumber Number

" 'self' keyword 
syn keyword nyxSelf self
hi def link nyxSelf Constant

" Operators
syn match nyxOperator display "\%(+\|-\|/\|*\|=\|\^\|&\||\|!\|>\|<\|%\)=\?"
syn match nyxOperator display "&&\|||"
hi def link nyxOperator Operator

" Function Definitions
syn match nyxFuncDef "\%(fn\s\+\)\@<=\w\+"
hi def link nyxFuncDef Function

" Function / Method Calls
syn match nyxFuncCall "\w\+\ze\s*("
hi def link nyxFuncCall Function

" Parameters and Struct Fields (Highlights words right before a colon)
syn match nyxField "\w\+\ze\s*:"
hi def link nyxField Identifier

" Comments
syn match nyxLineComment "\/\/.*$"
syn region nyxBlockComment start="/\*" end="\*/"
hi def link nyxLineComment Comment
hi def link nyxBlockComment Comment

" Strings and Interpolation
syn match nyxInterpolation "{[a-zA-Z_][a-zA-Z0-9_]*}" contained
syn region nyxString start='"' end='"' skip='\\"' contains=nyxInterpolation
hi def link nyxInterpolation Identifier
hi def link nyxString String

let b:current_syntax = "nyx"
