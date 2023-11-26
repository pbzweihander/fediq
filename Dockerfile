FROM docker.io/node:21 AS node
WORKDIR app

FROM docker.io/lukemathwalker/cargo-chef:latest-rust-1.73 AS chef
WORKDIR app

FROM node AS css-builder

COPY templates templates
COPY package.json package.json
COPY tailwind.config.js tailwind.config.js
COPY yarn.lock yarn.lock

RUN yarn && yarn run tailwindcss

FROM chef AS planner

COPY Cargo.toml Cargo.toml
COPY Cargo.lock Cargo.lock
RUN mkdir src && touch src/main.rs

RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY locales locales
COPY src src
COPY templates templates
COPY build.rs build.rs
COPY Cargo.lock Cargo.lock
COPY Cargo.toml Cargo.toml
COPY --from=css-builder /app/dist dist

ENV SKIP_TAILWINDCSS=1

RUN cargo build --release

FROM docker.io/debian:stable-slim AS runtime

COPY --from=builder /app/target/release/fediq /app/target/release/fediq-poster /usr/local/bin/

CMD ["fediq"]
