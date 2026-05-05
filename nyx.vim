" autocmd BufRead,BufNewFile *.nyx set filetype=nyx

if exists("b:current_syntax")
  finish
endif

" Keywords
syn keyword nyxKeyword fn let mut if else return while for struct inline const pub use
hi def link nyxKeyword Keyword

" Types
syn keyword nyxType i8 u8 i16 u16 i32 i64 f32 f64 bool char String uptr iptr
hi def link nyxType Type

" Booleans
syn keyword nyxBoolean true false
hi def link nyxBoolean Boolean

" Comments (Line and Block)
syn match nyxLineComment "\/\/.*$"
syn region nyxBlockComment start="/\*" end="\*/"
hi def link nyxLineComment Comment
hi def link nyxBlockComment Comment

" Strings
syn match nyxInterpolation "{[a-zA-Z_][a-zA-Z0-9_]*}" contained
hi def link nyxInterpolation Identifier

syn region nyxString start='"' end='"' skip='\\"' contains=nyxInterpolation
hi def link nyxString String

" Numbers
syn match nyxNumber "\v<\d+>"
hi def link nyxNumber Number

" Function declaration (word after 'fn')
syn match nyxFunctionDecl "fn\s\+\zs\w\+"
hi def link nyxFunctionDecl Function

" Function Calls (any word followed by an opening parenthesis)
syn match nyxFunctionCall "\w\+\ze\s*("
hi def link nyxFunctionCall Function

" Namespace separator ::
syn match nyxNamespaceSep "::"
hi def link nyxNamespaceSep Operator

" Modules
syn match nyxNamespace "\w\+\ze::"
syn match nyxNamespace "\(::\)\@<=\w\+"
hi def link nyxNamespace Type

" Import items
syn region nyxImportBraces start="::\s*{" end="}" transparent contains=nyxImportItem
syn match nyxImportItem "\w\+" contained
hi def link nyxImportItem Identifier

let b:current_syntax = "nyx"
