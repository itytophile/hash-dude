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
docker build -f docker/Dockerfile.slave -t itytophile/slave .
```

```sh
docker build -f docker/Dockerfile.server -t itytophile/server .
```

## Why other Dockerfiles for CI?

Because using cargo vendor within a runner is not easy. The CI has to update crates.io index for each build.

## Monitoring

Initialize the swarm:

```
docker swarm init
```

Install [websocket-client](https://pypi.org/project/websocket-client/) for Python. Yeah there is not an image for the monitor.

After creating the swarm:

```sh
docker stack deploy --compose-file docker-compose.yml stackhash
python docker/monitor.py
```

To delete the stack:

```sh
docker stack rm stackhash
```
