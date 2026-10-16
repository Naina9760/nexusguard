# Security

## Reporting a problem

Please report a vulnerability privately through GitHub's "Report a vulnerability" button on the Security tab of this repository, and do not open a public issue. Include the version, how to reproduce it, and what an attacker gains.

## What to know when running it

- `nexusguard serve` has no authentication. It binds to `127.0.0.1` by default and rejects any request whose `Host` header is not `localhost`, `127.0.0.1` or `[::1]`, which stops a web page in your browser from reading it through DNS rebinding. If you bind it to another address it prints a warning, and you should put an authenticating proxy in front of it.
- The page is read-only. It cannot change rules, restart anything or run commands.
- `--exec PROGRAM` runs the program you name, directly and without a shell, with each event as JSON on its stdin. Event text can include an error string from the server being watched, so the program should treat stdin as untrusted input.
- `--webhook URL` sends every event (rule names, timestamps and the text of scrape errors) to that URL. It is off by default.
- Metrics URLs must start with `http://` or `https://`. A metrics page is read up to 8 MiB.
- The web server limits itself to 32 concurrent connections and 8 KiB of request headers, and sends a strict Content-Security-Policy.

## What is out of scope

NexusGuard reads a metrics endpoint you give it. It does not authenticate to that endpoint, so if your metrics page needs credentials, put a proxy in front of it. A malicious server could return a misleading metrics page and hide a stall. NexusGuard trusts the endpoint it is pointed at.
