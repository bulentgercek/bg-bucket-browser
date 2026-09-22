// Starts the development server for the app to load. Unlike the shell wrapper
// next to it, this runs on every platform: it is what the app itself launches.
//
// The server is started through the API rather than through its command line
// tool, because the tool binds keyboard shortcuts and puts the terminal into raw
// mode for them. Closing the app's window kills it hard, before it can restore
// the terminal, and the shell is left without echo or line editing.
//
// Started this way there is nothing to restore. Every setting still comes from
// the same configuration file.

import { createServer } from "vite";

const server = await createServer();
await server.listen();
server.printUrls();
