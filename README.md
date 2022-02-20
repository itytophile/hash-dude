# hash-dude

## Images

- `itytophile/hash-slave` The slave which cracks the hashes.
- `itytophile/hash-server` The server which shares equally the ranges to each slave to crack a hash. It offers an UI at http://localhost:3000
- `itytophile/monitor` The monitor which observes the server's request queue. It asks docker to scale the slave service replicas.

### Why is the slave linked to glibc ?

glibc is harder to deal with when we do static linking however it is much faster than musl (the binary is bigger though).

## Code

- `src/main.rs` The server.
- `src/server/to_dude.rs` The task that manages dudes.
- `src/server/to_slave.rs` The task that manages slaves.
- `src/slave.rs` The slave.
- `src/monitor.rs` The monitor.
- `src/alphabet.rs` Functions that help us to generate words to crack hashes.

## Run the server

```sh
cargo run
# or cargo run -- --help
```

`--release` is good but not necessary.

## Run the slave

```sh
cargo run --release --bin slave
# or cargo run --release --bin slave -- --help
```

We add the `--release` to make the slave fast.

## Build the docker images

Before building:

```sh
cargo vendor
```

Then:

```sh
docker build -f docker/Dockerfile.slave -t itytophile/hash-slave .
```

```sh
docker build -f docker/Dockerfile.server -t itytophile/hash-server .
```

## Why other Dockerfiles for CI?

Because using cargo vendor within a runner is not easy. The CI has to update crates.io index for each build.

## Monitoring

Initialize the swarm:

```
docker swarm init
```

After creating the swarm:

```sh
docker stack deploy --compose-file docker-compose.yml stackhash
```

To delete the stack:

```sh
docker stack rm stackhash
```
