# magsys

Небольшая Linux CLI-утилита для безопасного развёртывания dotfiles и системных конфигураций.

```sh
cargo build --release
cp dotfiles.example.toml dotfiles.toml
./target/release/magsys status
./target/release/magsys install --dry-run
./target/release/magsys install
```

Формат конфигурации:

```toml
[[entries]]
source = "files/bashrc"
target = "~/.bashrc"
mode = 420 # десятичная запись для 0644; по умолчанию 0644
kind = "link" # link или copy
```

`source` разрешается относительно каталога `dotfiles.toml` и обязан находиться внутри него. Для целей под `/etc` и `/usr` права повышаются только на отдельных операциях через `sudo`.
