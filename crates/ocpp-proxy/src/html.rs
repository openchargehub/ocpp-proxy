pub const WELCOME_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>EV Charger Proxy</title>
</head>
<body>
  <h1>EV Charger Proxy</h1>
  <p>Proxy your EV charger to multiple backends and log charging sessions.</p>
  <ul>
    <li><a href="/charger">/charger</a> (WebSocket for charger)</li>
    <li><a href="/backend?id=your_backend_id">/backend?id=your_backend_id</a> (WebSocket for backend)</li>
    <li><a href="/sessions">/sessions</a> (JSON session data)</li>
    <li><a href="/sessions.csv">/sessions.csv</a> (CSV session data)</li>
    <li><a href="/status">/status</a> (backend status and control owner)</li>
    <li><a href="/override">/override</a> (POST to override control owner)</li>
  </ul>
</body>
</html>
"#;
