# Live Serve-rs

Live Serve-rs is a lightweight and fast development server with automatic file reloading. It serves static files and supports WebSockets for file change notifications, making it ideal for web development.

## Features

- Lightweight HTTP server for static files
- Automatic reloading via WebSockets
- Integration with notify for file change detection
- Support for Single Page Applications (SPA)
- Compatible with Axum and Tokio

## Installation

### Using Cargo

If you have Rust installed, you can install liveserve-rs directly from [crates.io](https://crates.io/crates/liveserve-rs):

```shell
cargo install liveserve-rs
```

### Using Precompiled Binaries

Precompiled binaries are available on [GitHub Releases](https://github.com/your-username/liveserve-rs/releases). To install:

1. Download the binary for your operating system.
2. Grant execution permissions (Linux/macOS):
   
   ```shell
   chmod +x liveserve-rs
   ```
   
3. Move the binary to a directory in your PATH, e.g.:
  
   ```shell
   mv liveserve-rs /usr/local/bin/
   ```

   
## Usage

### Serving a Directory

To serve a directory (e.g., ./public) with automatic reloading:

```shell
liveserve-rs --root ./public
```

### Specifying a Port

By default, the server runs on port 8080, but you can change it:

```shell
liveserve-rs --port 3000
```

### Configuring SPA Entry File

If you are running a Single Page Application (SPA) that uses index.html as a fallback:

```shell
liveserve-rs --spa-entry index.html
```

## Development

If you want to contribute or run the project locally:

```shell
git clone https://github.com/your-username/liveserve-rs.git
cd liveserve-rs
cargo run -- --root ./public
```

## License

This project is licensed under the [MIT License](LICENSE).
