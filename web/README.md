# mock-resend console

A viewer over mock-resend's admin API: every message your code sent, in whichever form
you need to look at it, and none of them delivered.

## Running it

The binary serves the built console at `/_admin/console/`, on the same port as the Resend
API. Open <http://localhost:9100> and it redirects there.

For working on the console itself:

```bash
pnpm install
pnpm dev
```

That serves the app on `:5173` and proxies `/_admin` to a mock-resend on `:9100`, so the
app talks to one origin and needs no CORS. Start the server separately with `cargo run`.

## Building it

```bash
pnpm build
```

Writes `dist/`, which `rust-embed` compiles into the binary. `cargo xtask dist` does both.

## Why `/_admin/console/` and not `/`

mock-resend serves the real Resend API on the same port, and Resend owns `/emails/{id}`.
A console route of that shape at the root would collide with the API the console exists
to show. Everything under `/_admin` is ours by definition.
