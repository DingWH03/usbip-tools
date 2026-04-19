# usbip-tools

这个仓库包含两个工程：

- `usbip-server`: Web 管理 + 规则持久化 + udev 热插拔自动绑定
- `usbip-client`: 用于远端 attach/detach

## UDP 发现（局域网广播）

- **端口**：`3240/udp`（与 usbip 的 `3240/tcp` 端口号复用，不冲突）
- **请求**：发送 `USBIP_DISCOVER\n`
- **响应**：JSON（包含 `server_name`、`web_url`、`version`），`web_url` 的 IP 会按“对方看到的本机源 IP”推导，适配多网卡场景。

## usbip-client 运行时自动提权

`usbip-client` 需要 root 权限来 `modprobe vhci-hcd` 和执行 `usbip attach`。为兼容后续 Windows 支持，客户端本身保持普通用户运行，仅在执行这些需要特权的命令时按需提权：

- 优先使用 `pkexec` 弹出提权对话框
- 如果没有 `pkexec` 或失败，再尝试 `sudo -E`
- 一次选择多个设备 attach 时，会在**单次提权**内顺序执行 `modprobe`（若需要）与多次 `usbip attach`，避免每个设备各弹一次密码

如果你的系统没有 polkit agent，安装后再试：

```bash
sudo apt-get install -y policykit-1
```

## systemd 安装

参考：`usbip-server/packaging/usbip-server.service`

```bash
# 构建并安装二进制
cargo build -p usbip-server --release
sudo install -m 0755 target/release/usbip-server /usr/local/bin/usbip-server

# 安装 systemd unit
sudo install -m 0644 usbip-server/packaging/usbip-server.service /etc/systemd/system/usbip-server.service

sudo systemctl daemon-reload
sudo systemctl enable --now usbip-server.service
```

## 生成 deb

debian13

依赖：
- `cargo-deb`（脚本会自动 `cargo install cargo-deb`）
- `zig`（用于 arm64 交叉编译，Debian 可 `sudo apt-get install zig`）
- `cargo-zigbuild`（脚本会自动 `cargo install cargo-zigbuild`）
- arm64 交叉依赖（用于 `libudev-sys` 的 pkg-config）：

```bash
sudo dpkg --add-architecture arm64
sudo apt-get update
sudo apt-get install -y pkg-config libudev-dev:arm64
```

```bash
make deb
ls -la dist/
```

只生成单一架构：

```bash
DEB_ARCHES=amd64 make deb
```

强制两种架构：

```bash
STRICT=1 make deb
```

## 注意

服务端需要启用3240端口tcp/udp访问

客户端需要启用源端口为 3240 的 UDP 包进入