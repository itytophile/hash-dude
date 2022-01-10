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

## Monitoring

After creating the swarm:

```sh
docker stack deploy --compose-file docker-compose.yml stackhash
python docker/monitor.py
```

To delete the stack:

```sh
docker stack rm stackhash
```
