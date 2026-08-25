import { createServer } from "node:net";

const port = 3000;
const server = createServer();
server.once("error", (error) => {
  if (error.code === "EADDRINUSE") {
    console.error(`Tauri development port ${port} is already in use. Stop the stale pnpm/Nuxt process before running pnpm tauri:dev.`);
  } else {
    console.error(`Could not check Tauri development port ${port}: ${error.message}`);
  }
  process.exitCode = 1;
});
server.once("listening", () => server.close());
server.listen(port, "127.0.0.1");
