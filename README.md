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
[[link]]
source = "niri"
target = "~/.config/niri"

[[config]]
source = "zramen/zramen.conf"
target = "/etc/conf.d/zramen"
mode = 0o644 # необязательно, по умолчанию 0644
sudo = true

[[script]]
source = "openrc/internal-keyboard-guard"
target = "/etc/init.d/internal-keyboard-guard"
mode = 0o755 # необязательно, по умолчанию 0755
sudo = true
```

Без `--config` файл `dotfiles.toml` ищется от текущего каталога вверх по дереву. `source` разрешается относительно каталога конфигурации и обязан находиться внутри него. `config` запрещает executable-биты, а `script` требует executable-бит владельца. Setuid, setgid и sticky запрещены. При `sudo = true` файл устанавливается как `root:root`, а права повышаются только для отдельных операций через `/usr/bin/sudo`; сам `magsys` нужно запускать от обычного пользователя. Недоступное без повышения прав содержимое отображается в `status` как `unreadable`.
