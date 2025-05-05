## Building Instructions

To build afplay for the web:

### Install wasm-pack and wasm-bindgen tools:

```bash
cargo [b]install wasm-pack wasm-bindgen-cli
```

### Build the package:

```bash
wasm-pack build --target web
```

### Use wasm-bindgen to create the JavaScript bindings:

```bash
wasm-bindgen target/wasm32-unknown-unknown/release/rust_wasm_minimal.wasm --out-dir ./examples/web/output --target web
```

## Run Instructions

### Install simple-http-server:
```bash
cargo [b]install simple-http-server
```

### Run examples/web content:

```bash
simple-http-server -i ./examples/web
```

Open a web browser at http://localhost:8000
