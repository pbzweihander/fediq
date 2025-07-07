FROM docker.io/node:22 AS css-builder
WORKDIR /app
COPY templates templates
COPY package.json package.json
COPY tailwind.config.js tailwind.config.js
COPY yarn.lock yarn.lock
RUN yarn && yarn run build-css

FROM docker.io/lukemathwalker/cargo-chef:latest-rust-1.88 AS chef
WORKDIR /app

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

RUN SKIP_TAILWINDCSS=1 cargo build --release

FROM docker.io/debian:stable-slim AS runtime

COPY --from=builder /app/target/release/fediq /app/target/release/fediq-poster /usr/local/bin/

CMD ["fediq"]
