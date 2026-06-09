" nyx basic syntax highlighting 
" semantic tokens with lsp setup is highly prefereable instead if supported in
" your code editor
"
" autocmd BufRead,BufNewFile *.nyx set filetype=nyx

if exists("b:current_syntax")
  finish
endif

" Keywords
syn keyword nyxKeyword fn let mut if else return while for struct enum inline const pub use impl with interface packed align match where
hi def link nyxKeyword Keyword

" Built-in Types
syn keyword nyxType i8 u8 i16 u16 i32 u32 i64 u64 f32 f64 bool char String str uptr iptr
hi def link nyxType Type

" Custom Types
syn match nyxStruct "\<[A-Z][a-zA-Z0-9_]*\>"
hi def link nyxStruct Type

" Constants
syn match nyxConstant "\<[A-Z][A-Z0-9_]*\>"
hi def link nyxConstant Constant

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
syn keyword nyxOperator as
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

" Constants (UPPER_SNAKE_CASE)
syn match nyxConstant "\<[A-Z][A-Z0-9_]*\>"
hi def link nyxConstant Constant

let b:current_syntax = "nyx"
