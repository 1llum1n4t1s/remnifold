# Requirements

Remote MCP clients connect to the deployed server without installing anything locally. The local app is a single native executable for your operating system and CPU; its MCP server, hooks, connection setup, history import and installer need no Node.js, Codex or Claude Code plugin system. Ordinary users install it with the Setup installer, which needs no command line and keeps itself up to date. The strict `remnifold install` route for advanced users requires authenticated native GitHub CLI. Registering it with Claude Code uses the Claude Code CLI when available; otherwise the exact command to run is printed.

Server setup and maintenance use the same native executable and your own Cloudflare account with Workers, D1, Vectorize, and Workers AI enabled. No Node.js, package manager, Wrangler, or Rust installation is required to use the platform package.
