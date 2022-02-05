## Images

- `itytophile/hash-slave` The slave which cracks the hashes.
- `itytophile/hash-server` The server which shares equally the ranges to each slave to crack a hash. It offers an UI at http://localhost:3000
- `itytophile/monitor` The monitor which observes the server's request queue. It asks docker to scale the slave service replicas.

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
