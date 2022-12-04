# Sentinel

Sentinel is a research purpose container runtime for lightweight serverless applications.
Details are described in [our paper](https://dl.acm.org/doi/10.1145/3565382.3565880).

## Prerequisites
We use [mold](https://github.com/rui314/mold) as the linker.
If you prefer other linkers, please edit `.cargo/config.toml`.
The evaluation on the paper was done on a 4 core (Intel Core i7 3.0 GHz) machine with Ubuntu 20.04 (Linux kernel version 5.14).

## Build
```bash
cargo +nightly build
```

## Run
Sentinel aims to be an OCI compatible runtime.
After creating a `config.json` file under your cwd
and creating a tap device `tap100` (Sentinel currently assumes a tap device with this name to support networking),
you should be able to run Sentinel with
```bash
sudo <path to sentinel executable> run <container id>
```

## Test
```bash
cargo +nightly test --workspace -- --test-threads 1
```

## Future Works
- Make Sentinel fully OCI compatible.
- More tests.
