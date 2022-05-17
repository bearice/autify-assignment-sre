#build
FROM rust:alpine
COPY . /src
RUN apk add openssl-dev musl-dev
RUN cd /src && cargo build --release

#package
FROM alpine
COPY --from=0 "/src/target/release/rget" /bin/rget
ENTRYPOINT ["/bin/rget"]
