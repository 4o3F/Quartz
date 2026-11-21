//! Stable CLI status output over the shared pipe client.

use quartz_pipe::{CallCoreError, CoreClient};
use quartz_protocol::CoreStatusResponse;
use tokio::io::{AsyncRead, AsyncWrite};

pub(crate) fn print_status<T: AsyncRead + AsyncWrite + Unpin>(
    mut client: CoreClient<T>,
) -> Result<(), CallCoreError> {
    let status = client.status()?;
    println!("{}", StatusReport { status: &status });
    Ok(())
}

struct StatusReport<'status> {
    status: &'status CoreStatusResponse,
}
impl std::fmt::Display for StatusReport<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let status = self.status;
        writeln!(formatter, "recording: {}", status.recording)?;
        writeln!(
            formatter,
            "stop_reason: {}",
            status
                .stop_reason
                .map_or("none", Into::<&'static str>::into)
        )?;
        writeln!(
            formatter,
            "database: {}",
            Into::<&'static str>::into(status.database)
        )?;
        writeln!(
            formatter,
            "browser_streams: {}",
            status.browser_streams.len()
        )?;
        writeln!(formatter, "instance_id: {}", status.instance_id.0)?;
        write!(formatter, "core_version: {}", status.core_version.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_report_has_stable_keys_and_enum_values() {
        let status =
            crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("test-instance".into()));
        let receiver = status.subscribe();
        let snapshot = receiver.borrow();
        assert_eq!(
            StatusReport {
                status: &snapshot.status
            }
            .to_string(),
            concat!(
                "recording: false\n",
                "stop_reason: starting\n",
                "database: ok\n",
                "browser_streams: 0\n",
                "instance_id: test-instance\n",
                "core_version: ",
                env!("CARGO_PKG_VERSION")
            )
        );
    }
}
