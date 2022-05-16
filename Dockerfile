#build
FROM rust:1.57
COPY . /src
RUN cd /src && cargo build --release

#package
FROM busybox
COPY --from=0 "/src/target/release/rget" /bin/rget
ENTRYPOINT ["/bin/rget"]
