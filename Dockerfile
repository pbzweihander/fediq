FROM docker.io/lukemathwalker/cargo-chef:latest-rust-1.88 AS chef
WORKDIR /app

FROM chef AS planner

COPY Cargo.toml Cargo.toml
COPY Cargo.lock Cargo.lock
RUN mkdir src && touch src/main.rs

RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder

RUN curl -LO https://github.com/tailwindlabs/tailwindcss/releases/latest/download/tailwindcss-linux-x64 &&\
    chmod +x tailwindcss-linux-x64 &&\
    mv tailwindcss-linux-x64 /usr/local/bin/tailwindcss

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY tailwind.config.js tailwind.config.js
COPY locales locales
COPY src src
COPY templates templates
COPY build.rs build.rs
COPY Cargo.lock Cargo.lock
COPY Cargo.toml Cargo.toml

RUN cargo build --release

FROM docker.io/debian:stable-slim AS runtime

COPY --from=builder /app/target/release/fediq /app/target/release/fediq-poster /usr/local/bin/

CMD ["fediq"]
