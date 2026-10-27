import App from "./App.svelte";
import { mount } from "svelte";

// Opened in a plain browser during development: preview the UI against a
// mock backend.
if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
  const { installMockBackend } = await import("./dev/mock");
  installMockBackend();
}

const app = mount(App, { target: document.getElementById("app")! });

export default app;
