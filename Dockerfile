# Build a static (musl) binary, then ship it alone in an empty image.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /app

# Compile dependencies first so code changes rebuild fast.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src

COPY src src
COPY static static
RUN touch src/main.rs && cargo build --release
# Where finished games are saved. Mount a volume here to keep them.
RUN mkdir /data

FROM scratch
COPY --from=build /app/target/release/bacteria /bacteria
COPY --from=build --chown=10001:10001 /data /data
ENV PORT=3000 DATA_DIR=/data
VOLUME /data
EXPOSE 3000
USER 10001
ENTRYPOINT ["/bacteria"]
