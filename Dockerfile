# A statically linked binary on a minimal base. Start-up time and image size are
# features, not incidental: this runs in CI, where it is pulled and started constantly.
#
# `rustls` rather than the platform TLS stack is what makes a fully static musl build
# possible at all — there is no OpenSSL to link against.

# The console first, because `rust-embed` compiles `web/dist` into the binary. Without
# this stage the image would ship a page saying the console was never built.
FROM node:24-alpine AS console

# Corepack reads `packageManager` from web/package.json, so the image uses the exact
# pnpm that wrote the lockfile. Letting it pick whatever is current means a different
# tool from the one the lockfile was resolved with, which disagrees about everything
# from the lockfile format to which packages its supply-chain policy will accept.
RUN corepack enable

WORKDIR /build/web

# Manifests first: the dependency install is the slow half and it only changes when they
# do. This list is two files, so it does not rot the way a per-crate list would.
COPY web/package.json web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile

COPY web/ ./
RUN pnpm run build

FROM rust:1.100-alpine AS builder

# `musl-dev` for the C runtime the linker needs. Nothing in the workspace has a C
# dependency, so this is the whole list.
RUN apk add --no-cache musl-dev

WORKDIR /build

# The whole tree in one layer. A manifest-first copy would cache dependency downloads
# better, but it means a list of every crate's `Cargo.toml` that silently rots the next
# time one is added, and the build then fails in a way nobody can read.
COPY . .

# Built in the stage above, and not in the context: `web/dist` is gitignored.
COPY --from=console /build/web/dist ./web/dist

# `--locked` so the build uses the versions in `Cargo.lock` or fails. Only the server
# crate: the conformance suite needs a real Resend SDK and has no business in an image.
RUN cargo build --release --locked --package mock-resend

FROM alpine:3.21

# Only for verifying TLS when a webhook endpoint is `https`. Webhook delivery is the one
# outbound thing this server does.
RUN apk add --no-cache ca-certificates \
    && addgroup -S -g 10001 mock-resend \
    && adduser -S -u 10001 -G mock-resend mock-resend \
    && mkdir -p /data \
    && chown mock-resend:mock-resend /data

COPY --from=builder /build/target/release/mock-resend /usr/local/bin/mock-resend

# Captured mail is sometimes real data a developer pointed here by mistake, and this
# process has no reason to be root to store it.
USER mock-resend

# Where captured mail is written. Mount a volume here and it survives a restart, and the
# `.eml` files can be opened from the host.
ENV MOCK_RESEND_DATA=/data
VOLUME ["/data"]

# The Resend API — which also serves the admin API and the console — and SMTP.
EXPOSE 9100 1025

# All interfaces, because the point of the container is to be reachable from the host.
# Nothing here is authenticated, so keep it on a container network or localhost.
ENV MOCK_RESEND_HOST=0.0.0.0

# The binary checks itself, so the image needs no `curl` or `wget` to be healthy.
HEALTHCHECK --interval=10s --timeout=3s --start-period=2s --retries=3 \
    CMD ["/usr/local/bin/mock-resend", "--healthcheck"]

ENTRYPOINT ["/usr/local/bin/mock-resend"]
