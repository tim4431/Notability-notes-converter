// Converts notes off the main thread with the WebAssembly converter.
import init, { Archive, convert } from "./pkg/notability_web.js";

const ready = init();

self.onmessage = async ({ data }) => {
  try {
    await ready;
  } catch (error) {
    self.postMessage({
      type: "failed",
      id: data.id,
      message: `The converter could not be loaded: ${error.message ?? error}`,
    });
    return;
  }
  try {
    if (data.type === "convert") {
      const result = convert(new Uint8Array(data.buffer), data.name, data.format);
      const bytes = result.bytes;
      const warnings = result.warnings;
      result.free();
      self.postMessage({ type: "converted", id: data.id, bytes, warnings }, [bytes.buffer]);
    } else if (data.type === "zip") {
      const archive = new Archive();
      for (const entry of data.entries) {
        archive.add(entry.name, entry.bytes);
      }
      const bytes = archive.finish();
      self.postMessage({ type: "zipped", id: data.id, bytes }, [bytes.buffer]);
    }
  } catch (error) {
    // A trap leaves the WebAssembly instance unusable, so the page starts a new worker.
    self.postMessage({
      type: "failed",
      id: data.id,
      message: error.message ?? String(error),
      trapped: error instanceof WebAssembly.RuntimeError,
    });
  }
};
