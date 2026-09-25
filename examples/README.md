# Mettle examples

Run these commands from the repository root after installing the `mettle` CLI.
Use `mettle check <file>` to validate a file without running it. In VS Code,
open any `.mettle` file to use its flow/test play buttons.

## Language

The standalone language files below require no network access.

| Example | What it demonstrates | Try it |
| --- | --- | --- |
| [Basics](language/basics.mettle) | Reusable flows, implicit final value, and a test | `mettle run examples/language/basics.mettle` |
| [Conditionals](language/conditionals.mettle) | `if`/`else`, boolean logic, indexing, and terminal `fail()` | `mettle test examples/language/conditionals.mettle` |
| [Contexts](language/contexts.mettle) | Reusable and anonymous file-level contexts | `mettle run examples/language/contexts.mettle` |
| [Collections and numbers](language/collections-and-numbers.mettle) | Named parallel work, `for` mapping, retries, numeric literals | `mettle test examples/language/collections-and-numbers.mettle` |
| [Echo](language/echo.mettle) | Report messages and named parallel labels | `mettle run examples/language/echo.mettle` |
| [Secrets](language/secrets.mettle) | `senv()` and redaction | `mettle run examples/language/secrets.mettle` |
| [Standalone profiles](language/profiles/main.mettle) | Automatic `.env` and `--profile` overlays | `mettle run examples/language/profiles/main.mettle --profile qa` |

Set `API_TOKEN` to any disposable demo value before running the secrets example.

The [multi-file project](language/project/main.mettle) demonstrates a project
manifest, namespaces, contexts, and profiles. It makes one request to the
public JSONPlaceholder API:

```bash
mettle run examples/language/project/main.mettle --profile qa
```

The profile and project `.env` files contain only public demonstration values;
they are intentionally allowlisted in `.gitignore`.

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
