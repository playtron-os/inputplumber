# inputplumber-client

A small Rust client for InputPlumber's `org.shadowblip.InputManager` DBus
interface, built on zbus 5. It holds interface definitions only, written new
for this crate, and is MIT licensed so that programs under any licence can
talk to the daemon. The daemon itself stays GPL-3.0-or-later.

It covers the whole Manager interface, including target groups:

- `TargetGroups` and `ActiveTargetGroup` properties
- `AddTargetGroup` and `RemoveTargetGroup` methods

Each composite device keeps one set of gamepad targets per target group. Only
the active group's gamepads receive input. The other groups' gamepads stay
connected and idle. Each group's view of `/dev/input` is kept under
[`VIEWS_PATH`](src/lib.rs), so a sandbox or container can bind that view as
its own `/dev/input`.

```rust,no_run
# async fn run() -> zbus::Result<()> {
let bus = zbus::Connection::system().await?;
let manager = inputplumber_client::InputManagerProxy::new(&bus).await?;
manager.add_target_group("desktop").await?;
manager.set_active_target_group("desktop").await?;
# Ok(())
# }
```

This crate is part of Playtron's InputPlumber fork and is offered upstream
along with the target groups feature.
