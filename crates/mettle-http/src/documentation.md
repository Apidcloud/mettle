No request is performed when opening this reference. On methods accepting a body,
an omitted body is different from JSON null.
`mediaType` or a `Content-Type` header selects an encoder for ordinary values.
JSON and `+json` use generic JSON processing; `text/*` uses UTF-8 text. Unknown
representations require already encoded bytes or a source. Media-type names alone
do not define a domain schema or install a processor.

Bytes and sources are never encoded again. `body: json.encode(value)` sends the
encoded bytes unchanged; use `mediaType: json.mediaType` to label them. For another
charset, send explicitly encoded bytes. No automatic compression is performed.
`bodyFormat` is no longer supported.

When both `mediaType` and `Content-Type` are supplied, their parsed values must
agree. Malformed types, duplicate parameters/Content-Type headers, unsupported
encoding charsets, or conflicting options fail before a source is consumed.

Uploads from `fs.stream(path)` are single-consumer sources. Framing is automatic:
do not supply `Content-Length` or `Transfer-Encoding` for a source. An early final
response stops production. Source failures preserve their source location.
Cancellation stops owned work; sources are never automatically replayed. An
explicit retry must create a new source on each attempt, and can duplicate remote
side effects.

Incoming `.body` is decoded into native values: JSON/`+json` produces an object,
array, scalar, or null; `text/*` produces a strict UTF-8 string; missing/unknown
content types produce bytes, without sniffing. `.bodyBytes` retains bounded
representation bytes. `.mediaType` is the normalized Content-Type string with
explicit parameters, or null when absent; the original header remains available.
JSON is a representation, not a native kind or proof of application fields.

HEAD and statuses 204/205/304 have null bodies. Other empty text and binary
representations remain an empty string or bytes; empty declared JSON is invalid.
Malformed or duplicate Content-Type, invalid JSON/UTF-8, unsupported JSON/text
charsets, numeric overflow, and body limits fail with source-aware errors.
Non-identity Content-Encoding is rejected for responses with a body; automatic
decompression is not implemented. Error HTTP statuses still return normally
when their metadata/content are valid.

Response `.json` is removed: use `.body` for decoded values or explicitly decode
`.bodyBytes`. HTTP/2, compression, live response iteration, and user codecs remain
future work. Variable/field inference and richer hovers are the separate phase 3.5.
