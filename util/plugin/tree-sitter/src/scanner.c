#include "tree_sitter/parser.h"
#include <string.h>

enum TokenType {
  NEWLINE, CALL_CONTINUATION, MEMBER_CONTINUATION, INDEX_CONTINUATION, OR_CONTINUATION, AND_CONTINUATION,
  COMPARISON_CONTINUATION, CALL_OPTIONS_START, ELSE_START, ERROR_SENTINEL
};

void *tree_sitter_mettle_external_scanner_create(void) { return NULL; }
void tree_sitter_mettle_external_scanner_destroy(void *payload) { (void)payload; }
unsigned tree_sitter_mettle_external_scanner_serialize(void *payload, char *buffer) {
  (void)payload; (void)buffer; return 0;
}
void tree_sitter_mettle_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
  (void)payload; (void)buffer; (void)length;
}

// The compiler ignores whitespace while parsing expressions, but requires a
// newline between statements (or a newline/comma between fields and branches).
// Look ahead without consuming the gap so comments remain visible tree nodes.
bool tree_sitter_mettle_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
  (void)payload;
  if (valid_symbols[ERROR_SENTINEL]) return false;
  lexer->mark_end(lexer);
  bool newline = false;
  for (;;) {
    if (lexer->lookahead == ' ' || lexer->lookahead == '\t' || lexer->lookahead == '\r' || lexer->lookahead == '\n') {
      newline |= lexer->lookahead == '\n';
      lexer->advance(lexer, true);
    } else if (lexer->lookahead == '/') {
      lexer->advance(lexer, true);
      if (lexer->lookahead != '/') return false;
      while (lexer->lookahead && lexer->lookahead != '\n') lexer->advance(lexer, true);
    } else break;
  }
  // Signal continuations only where the grammar allows them. In particular,
  // `{` continues a call's options, but can start a new object statement after
  // a different value. All tokens here are zero-width; the normal lexer keeps
  // ownership of the actual punctuation, keywords, whitespace and comments.
  enum TokenType continuation = ERROR_SENTINEL;
  const int32_t next = lexer->lookahead;
  switch (next) {
    case '.': continuation = MEMBER_CONTINUATION; break;
    case '[': continuation = INDEX_CONTINUATION; break;
    case '(': continuation = CALL_CONTINUATION; break;
    case '<': case '>': case '!':
      continuation = COMPARISON_CONTINUATION;
      break;
    case '{':
      if (valid_symbols[CALL_OPTIONS_START]) {
        lexer->result_symbol = CALL_OPTIONS_START;
        return true;
      }
      break;
    case '=':
      lexer->advance(lexer, true);
      if (lexer->lookahead == '=') continuation = COMPARISON_CONTINUATION;
      break;
    default: {
      char word[6] = {0};
      unsigned i = 0;
      while ((lexer->lookahead >= 'a' && lexer->lookahead <= 'z') && i < sizeof(word) - 1) {
        word[i++] = (char)lexer->lookahead;
        lexer->advance(lexer, true);
      }
      bool boundary = !((lexer->lookahead >= 'a' && lexer->lookahead <= 'z') ||
        (lexer->lookahead >= 'A' && lexer->lookahead <= 'Z') ||
        (lexer->lookahead >= '0' && lexer->lookahead <= '9') || lexer->lookahead == '_');
      if (boundary && !strcmp(word, "else") && valid_symbols[ELSE_START]) {
        lexer->result_symbol = ELSE_START;
        return true;
      }
      if (boundary && !strcmp(word, "and")) continuation = AND_CONTINUATION;
      if (boundary && !strcmp(word, "or")) continuation = OR_CONTINUATION;
    }
  }
  if (continuation != ERROR_SENTINEL && valid_symbols[continuation]) {
    lexer->result_symbol = continuation;
    return true;
  }
  // A comma on the following line is still the explicit field/branch separator.
  if (next == ',') return false;
  if (newline && valid_symbols[NEWLINE]) {
    lexer->result_symbol = NEWLINE;
    return true;
  }
  return false;
}
