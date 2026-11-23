// Static landing page for `/` (the phone client lives under `/<device-id>`).

export function landing(): string {
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Worse Harness Remote Control</title>
<style>
  :root { color-scheme: dark; }
  body { margin: 0; min-height: 100vh; display: grid; place-items: center; background: #0b0b0d;
         color: #e7e7ea; font: 15px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif; }
  main { max-width: 420px; padding: 32px; border: 1px solid #26262b; border-radius: 16px; background: #131317; }
  h1 { font-size: 20px; margin: 0 0 8px; }
  p { margin: 0 0 12px; color: #a1a1aa; }
  a { color: #8ab4ff; }
</style>
</head>
<body>
<main>
  <h1>Worse Harness Remote Control</h1>
  <p>Continue your work from your phone. In Harness on your computer, open
     <b>Settings › Remote</b>, turn on <b>Enable Remote Control</b> and scan the QR code.</p>
  <p>Messages are end-to-end encrypted between your phone and your computer; this relay only forwards them.</p>
  <p><a href="https://github.com/rraahh133/Worse">github.com/rraahh133/Worse</a></p>
</main>
</body>
</html>`;
}
