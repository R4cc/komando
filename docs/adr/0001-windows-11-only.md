# Windows 11 is the only supported platform

Komando runs on Windows 11 and nowhere else. Its users sit on Windows workstations and connect to Linux (mostly RHEL) servers. Supporting only one OS lets us use Windows-specific features directly (ConPTY, DirectWrite, DPAPI, Windows Hello) without cross-platform abstractions. We will not add macOS or Linux builds.
