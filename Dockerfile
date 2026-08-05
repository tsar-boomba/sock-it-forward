FROM --platform=$BUILDPLATFORM debian:bullseye-slim AS chef
WORKDIR /app

# Update default packages
RUN apt-get update

# Get Ubuntu packages
RUN apt-get install -y \
    build-essential \
    curl

# Update new packages
RUN apt-get update

# Get Rust and cargo-chef
RUN curl https://sh.rustup.rs -sSf | bash -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"
RUN cargo install --locked --version 0.1.77 cargo-chef

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder 
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies - this is the caching Docker layer!
RUN cargo chef cook --release --recipe-path recipe.json
# Build application
COPY src src
COPY Cargo.toml Cargo.toml
COPY Cargo.lock Cargo.lock
RUN cargo build --release

# Fetch a static dumb-init binary for the target arch.
# uname -m yields x86_64 / aarch64, which matches dumb-init's release asset names.
RUN curl -fsSL "https://github.com/Yelp/dumb-init/releases/download/v1.2.5/dumb-init_1.2.5_$(uname -m)" \
    -o /usr/local/bin/dumb-init \
    && chmod +x /usr/local/bin/dumb-init

FROM --platform=$BUILDPLATFORM gcr.io/distroless/cc-debian12 AS runtime
WORKDIR /app
COPY --from=builder /usr/local/bin/dumb-init /usr/local/bin/dumb-init
COPY --from=builder /app/target/release/sock-it-forward /sock-it-forward
ENTRYPOINT ["/usr/local/bin/dumb-init", "--", "/sock-it-forward"]
