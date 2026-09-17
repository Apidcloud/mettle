# Flow: one language from API scratchpad to load test

Today, a quick API call lives in a `.http` file or a collection. A real workflow moves into a script. Load testing moves again into another tool. The same API logic gets rewritten at every step.

Flow keeps it in one place.

```flow
// Start as simply as a .http file.
http.get("https://api.example.com/health")

// Grow into a reusable workflow when the request needs data and logic.
flow createUser() {
    seed = http.get("https://api.example.com/seed")

    return http.post("https://api.example.com/users") {
        json: {
            name: seed.json.name
            sourceId: seed.json.id
        }
    }
}
```

The same flow can become an integration check or a performance scenario—without translating it into a different DSL:

```flow
// Proposed test/load layer.
test("user creation stays fast") {
    load = rate(target: 1_000, period: 1s, duration: 30s) {
        createUser()
    }

    assert(load.latency.p95 < 200ms)
}
```

Why it is different:

- **One workflow, no rewrites:** scratch request → reusable flow → test → load scenario.
- **Data flows naturally:** responses are values, so one request can drive the next without glue code.
- **Compiler-backed:** validate calls, options, contexts, and arguments before traffic is sent.
- **Built for serious I/O:** native Rust runtime, reusable HTTP connections, bounded execution, and metrics designed into the runtime rather than bolted on.

The HTTP MVP already runs with HTTPS, JSON, contexts, a CLI, and VS Code actions. Assertions, parallelism, and load scheduling are the next layers, built on the same compiled flow model.
