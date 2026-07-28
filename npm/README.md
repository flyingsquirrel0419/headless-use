# headless-use for npm

This package contains the `headless-use` Linux x86_64 binary. It does not use
an install script or download executable code during installation.

```bash
npm install --global headless-use
headless-use doctor
```

Chrome or Chromium must be installed on the host. Set
`HEADLESS_USE_BROWSER_PATH` when it is not discoverable on `PATH`. For an image
that includes Chromium, use the container distribution documented in the
[main README](https://github.com/flyingsquirrel0419/headless-use#install).

Only Linux x86_64 is supported by this npm package in v1.
