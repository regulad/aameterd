FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
RUN cargo build --release --locked

FROM alpine:3
COPY --from=build /src/target/release/aameterd /usr/local/bin/aameterd
ENV AAMETERD_LISTEN=0.0.0.0:8182 AAMETERD_STATS_LISTEN=0.0.0.0:8183 AAMETERD_DB=/data/aameterd.sqlite3
VOLUME /data
EXPOSE 8182 8183
CMD ["sh", "-c", "exec aameterd --upstream \"http://${AAORACLED_HOST:?set AAORACLED_HOST to the aaoracled IP}:8181\""]
