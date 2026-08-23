//! Desktop notifications for things that happen while nobody is watching —
//! chiefly the headless `--scheduled-backup` run, whose only other output is
//! the log file. Uses the WinRT toast API through Windows PowerShell so there
//! is no extra dependency; the toast is attributed to PowerShell's own
//! AppUserModelID because an unpackaged exe has none registered, and shows
//! "Disk Utility" as its first line instead.

use crate::logger;
use crate::ops;

/// AUMID every Windows install has; required for `CreateToastNotifier`.
const AUMID: &str = r"{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe";

/// Show a toast with `title` and `body`. Failures are logged, never fatal —
/// a missing notification must not turn a finished backup into an error.
pub fn toast(title: &str, body: &str) {
    let script = format!(
        "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType=WindowsRuntime] | Out-Null
         [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType=WindowsRuntime] | Out-Null
         $xml = New-Object Windows.Data.Xml.Dom.XmlDocument
         $xml.LoadXml('{xml}')
         $t = [Windows.UI.Notifications.ToastNotification]::new($xml)
         [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{aumid}').Show($t)",
        xml = ops::ps_quote(&toast_xml(title, body)),
        aumid = ops::ps_quote(AUMID),
    );
    if let Err(e) = ops::run_ps_quiet(&script) {
        logger::log(format!("notify: toast failed: {e}"));
    }
}

/// ToastGeneric payload: app name, title, body (body truncated so a long
/// error message still fits the notification).
fn toast_xml(title: &str, body: &str) -> String {
    let body: String = body.chars().take(300).collect();
    format!(
        "<toast scenario=\"reminder\"><visual><binding template=\"ToastGeneric\">\
         <text>Disk Utility</text><text>{}</text><text>{}</text>\
         </binding></visual></toast>",
        xml_escape(title),
        xml_escape(&body)
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_is_escaped_and_truncated() {
        let x = toast_xml("a<b", &"é".repeat(400));
        assert!(x.contains("<text>a&lt;b</text>"));
        assert_eq!(x.matches('é').count(), 300);
    }
}
