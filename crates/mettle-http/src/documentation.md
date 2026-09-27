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

Incoming `.body` is still text, `.json` is parsed JSON, and `.bodyBytes` is raw
representation bytes. Empty bodies and unavailable JSON expose `.json` as null;
malformed nonempty declared JSON fails. Incoming body normalization, HTTP/2,
compression, live response iteration, and user codecs remain future work.
