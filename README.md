# MineCLI - MineChat Client

[![CI status](https://github.com/MineChatPowered/client/actions/workflows/rust.yml/badge.svg)](https://github.com/MineChatPowered/client/actions/workflows/rust.yml)

This repo is for the CLI client, which enables you to chat on a Minecraft server without actually logging in to the Minecraft account itself.

Related projects:
- [MineChatPowered/server](https://github.com/MineChatPowered/server): the Minecraft plugin which acts as the server-side component of MineChat
- [MineChatPowered/protocol-rs](https://github.com/MineChatPowered/protocol-rs): the Rust library which lays down the base and helper functions to use the protocol

*Not affiliated with or endorsed by Mojang Studios or Minecraft.*

## Features

- **Link your account:** Link your client with a MineChat server using a provided code.
- **Real-time chat:** Send and receive chat messages in real time.
- **Persistent server configuration:** Stores linked server configurations locally in a JSON file, making repeated connections easier.

## Installation

To build the client, you need to [install](https://www.rust-lang.org/tools/install) the Rust compiler toolchain.

1. **Clone the repository:**

   ```bash
   git clone https://github.com/MineChatPowered/client.git
   cd minechat-client
   ```

2. **Build the project:**

   ```bash
   cargo build --release
   ```

   The built binary will be located in `target/release/minechat-client`.

## Usage

Before running the client, ensure that the server part is running either locally or on a remote server:

- If you run the server, download the [server plugin](https://github.com/MineChatPowered/server/releases/latest).
- Start the server which contains the server part of the platform.
- Log on to the Minecraft server and run `/link` to generate a code.
- Use the generated code to link your account with the Minecraft server.

### Logging on from the CLI

#### Linking Your Account

To link your account with a MineChat server using a provided code:

```bash
minechat-client --server <host:port> --link <code>
```

#### Connecting to a Server

If your server is already linked, simply connect:

```bash
minechat-client --server <host:port>
```

#### Enabling Verbose Logging

To see detailed debug and log outputs, include the verbose flag:

```bash
minechat-client --server <host:port> --verbose
```

## Configuration

The client saves server entries in a JSON configuration file. The configuration file is placed in the default configuration directory provided by the OS. The file is named `servers.json` and includes entries like:

```json
{
  "servers": [
    {
      "address": "localhost:25575",
      "uuid": "your-client-uuid"
    }
  ]
}
```

Each entry represents a server you have linked with a unique client UUID.

## Contributing

Contributions are welcome! Feel free to open issues or pull requests on the [GitHub repository](https://github.com/MineChatPowered/client).

### Roadmap

- [ ] Execute commands on the server.

## License

This project is licensed under the terms of the MPL-2.0 license. See the [license file](LICENSE) for details.
