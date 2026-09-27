# Mettle examples

Run these commands from the repository root after installing the `mettle` CLI.
Use `mettle check <file>` to validate a file without running it. In VS Code,
open any `.mettle` file to use its flow/test play buttons.

File-level batches are sequential by default unless `mettle.toml` sets a job
count. When their entries are
independent, add `--jobs N` to run a bounded number concurrently:

```bash
mettle test examples/http/tests.mettle --jobs 2
mettle run examples/http/requests.mettle --all --jobs 3
```

Parameterized flows are skipped by `run --all`. Keep `--jobs 1` for entries
that intentionally share or mutate external state.

## Language

The standalone language files below require no network access.

| Example | What it demonstrates | Try it |
| --- | --- | --- |
| [Basics](language/basics.mettle) | Reusable flows, implicit final value, and a test | `mettle run examples/language/basics.mettle` |
| [Conditionals](language/conditionals.mettle) | `if`/`else`, boolean logic, indexing, and terminal `fail()` | `mettle test examples/language/conditionals.mettle` |
| [Contexts](language/contexts.mettle) | Reusable and anonymous file-level contexts | `mettle run examples/language/contexts.mettle` |
| [Collections and numbers](language/collections-and-numbers.mettle) | Named parallel work, `for` mapping, retries, numeric literals | `mettle test examples/language/collections-and-numbers.mettle` |
| [Echo](language/echo.mettle) | Report messages and named parallel labels | `mettle run examples/language/echo.mettle` |
| [Top-level jobs](language/jobs.mettle) | Bounded concurrent flow and test batches | `mettle test examples/language/jobs.mettle --jobs 3` |
| [Secrets](language/secrets.mettle) | `senv()` and redaction | `mettle run examples/language/secrets.mettle` |
| [Standalone profiles](language/profiles/main.mettle) | Automatic `.env` and `--profile` overlays | `mettle run examples/language/profiles/main.mettle --profile qa` |
| [Complete file reads](language/files.mettle) | Reusable bytes and explicit UTF-8 text reads | `mettle run examples/language/files.mettle inspectFile --arg path=examples/language/filesystem/data/payload.txt` |
| [Filesystem project](language/filesystem/main.mettle) | Incremental copy, helper sources, and project `workingDir` | `mettle run examples/language/filesystem/main.mettle` |

Set `API_TOKEN` to any disposable demo value before running the secrets example.

The [multi-file project](language/project/main.mettle) demonstrates a project
manifest, namespaces, contexts, and profiles. It makes one request to the
public JSONPlaceholder API:

```bash
mettle run examples/language/project/main.mettle --profile qa
```

The same project's [network-free checks](language/project/checks.mettle)
demonstrate `[run].jobs` and `[test].jobs` defaults from its manifest:

```bash
mettle run examples/language/project/checks.mettle --all
mettle test examples/language/project/checks.mettle
mettle test examples/language/project/checks.mettle --jobs 1
```

The profile and project `.env` files contain only public demonstration values;
they are intentionally allowlisted in `.gitignore`.

The filesystem project writes `data/copy.txt`, an ignored demonstration output.
Its explicit `overwrite: true` permits repeated runs. Complete reads use bounded
memory; `fs.stream` supplies one incremental read. Standalone paths use the CLI
invocation directory; project paths use the root or configured `workingDir`.

## HTTP — local file uploads

[File upload](http/file-upload.mettle) demonstrates both buffered and streamed
request bodies, plus a server that rejects an upload before reading its body.
Start the included fixture on loopback:

```bash
python3 util/test-server/http_fixture.py --port 8080
```

In another terminal, from the repository root:

```bash
mettle run examples/http/file-upload.mettle upload \
  --arg path=examples/language/filesystem/data/payload.txt
mettle run examples/http/file-upload.mettle rejectedUpload \
  --arg path=examples/language/filesystem/data/payload.txt --raw > result.json
```

No exchange scope is needed: `http.post` drives the source and returns its usual
response. The fixture reports a byte count and SHA-256 digest rather than echoing
file contents. For VS Code's Run Flow action, provide an absolute file path or a
path relative to the active standalone file's directory.

## HTTP — internet connection required

These use the public JSONPlaceholder demo API. They do not need credentials.
Write requests are demonstrations; do not treat their responses as persistent
data. The load example is intentionally tiny and is not a benchmark.

| Example | What it demonstrates | Try it |
| --- | --- | --- |
| [Requests](http/requests.mettle) | Anonymous calls, flow parameters, and a POST body | `mettle list examples/http/requests.mettle` |
| [Workflow](http/workflow.mettle) | HTTP defaults, chaining, and assertions | `mettle run examples/http/workflow.mettle` |
| [Tests](http/tests.mettle) | Functional checks and test selection | `mettle test examples/http/tests.mettle` |
| [Policies](http/policies.mettle) | Multi-step retry, deadlines, and named parallel results | `mettle run examples/http/policies.mettle` |
| [Load](http/load.mettle) | A bounded, short rate workload | `mettle run examples/http/load.mettle` |
| [Parallel load](http/load-parallel.mettle) | Two concurrent rate workloads with per-action live metrics | `mettle run examples/http/load-parallel.mettle` |

`requests.mettle` has several runnable entries. Select one by name or inspect
source lines with `mettle list` before using `--line`:

```bash
mettle run examples/http/requests.mettle inspectRequest \
  --arg baseUrl=https://jsonplaceholder.typicode.com \
  --arg requestId=1
```
