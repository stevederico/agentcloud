# Zero-crate Rust binary. Links SQLCipher 4.18, built from the official tag.
# Bookworm's packages stop at 4.6.1.

# Base images are pinned by multi-arch index digest (tag kept for
# readability). Each digest was first published at least 7 days before
# it was pinned (2026-11-03).
# rust:1.99.0-slim-bookworm, pushed 2026-10-06 (1.99.1-slim-bookworm moved on 2026-10-28, < 7 days)
FROM rust:1.99.0-slim-bookworm@sha256:2c3a22f0a5533ea2dd5a16627bc841228151faa2d4de2644ac9987e4a2f1f2fa AS builder
WORKDIR /build

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        gcc libc6-dev make tcl libssl-dev curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# SQLCipher 4.7+ builds with autosetup: --with-tempstore and the EXTRA_INIT
# hooks are required, and the library installs as libsqlite3 (the symlink
# keeps build.rs's -lsqlcipher working).
ARG SQLCIPHER_VERSION=4.18.0
RUN curl -fsSL -o /tmp/sqlcipher.tar.gz \
        "https://github.com/sqlcipher/sqlcipher/archive/refs/tags/v${SQLCIPHER_VERSION}.tar.gz" \
    && tar -xzf /tmp/sqlcipher.tar.gz -C /tmp \
    && cd "/tmp/sqlcipher-${SQLCIPHER_VERSION}" \
    && ./configure --prefix=/usr/local --with-tempstore=yes \
        CFLAGS="-DSQLITE_HAS_CODEC -DSQLITE_EXTRA_INIT=sqlcipher_extra_init -DSQLITE_EXTRA_SHUTDOWN=sqlcipher_extra_shutdown" LDFLAGS="-lcrypto" \
    && make -j"$(nproc)" \
    && make install \
    && ln -sf libsqlite3.so /usr/local/lib/libsqlcipher.so \
    && ldconfig \
    && rm -rf /tmp/sqlcipher.tar.gz "/tmp/sqlcipher-${SQLCIPHER_VERSION}"

COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY public ./public

RUN cargo build --release

# debian:bookworm-20261020-slim, pushed 2026-10-21
FROM debian:bookworm-20261020-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8
WORKDIR /app
RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/lib/libsqlcipher.so /usr/local/lib/libsqlite3.so* /usr/local/lib/
RUN ldconfig

COPY --from=builder /build/target/release/agentcloud /usr/local/bin/agentcloud
# The old name, for anything that still runs it (the apps-1 restore test).
RUN ln -s agentcloud /usr/local/bin/almanac

ENV PORT=8000 \
    HOST=:: \
    DB_PATH=/app/data/calendar.db

EXPOSE 8000
CMD ["/usr/local/bin/agentcloud"]
