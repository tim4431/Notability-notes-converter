// The page: hands files one at a time to the converter worker and lists the results.
const MIME_TYPES = {
  pdf: "application/pdf",
  svg: "image/svg+xml",
  note: "application/octet-stream",
};
const NOTE_FILE = /\.(note|ntb)$/i;

const picker = document.getElementById("files");
const drop = document.getElementById("drop");
const section = document.querySelector(".results");
const list = document.getElementById("results");
const message = document.getElementById("message");
const downloadAll = document.getElementById("download-all");
const clear = document.getElementById("clear");

let items = [];
let nextId = 1;
let converting = null;
let worker = startWorker();

function startWorker() {
  const next = new Worker(new URL("worker.js", import.meta.url), { type: "module" });
  next.onmessage = ({ data }) => receive(data);
  next.onerror = (event) => {
    event.preventDefault();
    const reason = event.message || "unknown error";
    const item = converting;
    // Without a file to blame, a new worker would fail the same way.
    if (!item) {
      say(`The converter could not start: ${reason}`);
      return;
    }
    restartWorker();
    show(item, { error: `The converter stopped: ${reason}` });
    next();
  };
  return next;
}

// A new worker starts empty: the file it was converting goes back in the queue.
function restartWorker() {
  worker.terminate();
  worker = startWorker();
  if (converting) {
    converting.status = "queued";
    render(converting);
    converting = null;
  }
}

function format() {
  return document.querySelector('input[name="format"]:checked').value;
}

function addFiles(files) {
  for (const file of files) {
    const item = { id: nextId++, file, element: document.createElement("li") };
    list.append(item.element);
    items.push(item);
    if (NOTE_FILE.test(file.name)) {
      queue(item);
    } else {
      show(item, { error: "Not a .note or .ntb file" });
    }
  }
  section.hidden = items.length === 0;
  next();
}

function queue(item) {
  release(item);
  item.status = "queued";
  item.format = format();
  render(item);
}

// Converts the next queued file once the worker is free.
async function next() {
  if (converting) {
    return;
  }
  const item = items.find((candidate) => candidate.status === "queued");
  if (!item) {
    updateActions();
    return;
  }
  converting = item;
  item.status = "converting";
  render(item);
  let buffer;
  try {
    buffer = await item.file.arrayBuffer();
  } catch (error) {
    converted(item, { error: `The file could not be read: ${error.message}` });
    return;
  }
  if (item.status !== "converting") {
    converted(item, {});
    return;
  }
  worker.postMessage(
    { type: "convert", id: item.id, name: item.file.name, format: item.format, buffer },
    [buffer],
  );
}

function receive(data) {
  if (data.type === "zipped") {
    save(new Blob([data.bytes], { type: "application/zip" }), "Notability notes.zip");
    downloadAll.disabled = false;
    return;
  }
  if (data.id === "zip") {
    downloadAll.disabled = false;
    say(`The archive could not be made: ${data.message}`);
    if (data.trapped) {
      restartWorker();
      next();
    }
    return;
  }
  const item = items.find((candidate) => candidate.id === data.id);
  if (data.trapped) {
    converting = null;
    restartWorker();
  }
  converted(item, data.type === "converted" ? data : { error: data.message });
}

// Takes a result from the worker. A file that was cleared, or whose format changed,
// in the meantime is not shown; a changed one is converted again.
function converted(item, result) {
  converting = null;
  if (item?.status === "converting" && (result.bytes || result.error)) {
    show(item, result);
  }
  next();
}

function show(item, { bytes, warnings = [], error }) {
  item.status = error ? "failed" : "done";
  item.error = error;
  item.warnings = warnings;
  if (bytes) {
    item.bytes = bytes;
    item.outputName = item.file.name.replace(NOTE_FILE, "") + "." + item.format;
    item.url = URL.createObjectURL(new Blob([bytes], { type: MIME_TYPES[item.format] }));
  }
  render(item);
}

function render(item) {
  const { element } = item;
  element.className = item.status;
  element.replaceChildren();
  const name = document.createElement("span");
  name.className = "file";
  name.textContent = item.file.name;
  const status = document.createElement("span");
  status.className = "status";
  if (item.status === "queued") {
    status.textContent = "Waiting";
  } else if (item.status === "converting") {
    status.textContent = "Converting…";
  } else if (item.status === "failed") {
    status.textContent = item.error;
  } else {
    const unchanged = item.format === "note" && /\.note$/i.test(item.file.name);
    status.textContent = unchanged ? "Already a .note" : size(item.bytes.length);
  }
  element.append(name, status);
  if (item.url) {
    const link = document.createElement("a");
    link.href = item.url;
    link.download = item.outputName;
    link.textContent = `Download ${item.format === "note" ? ".note" : item.format.toUpperCase()}`;
    element.append(link);
  }
  if (item.warnings?.length) {
    const warnings = document.createElement("ul");
    warnings.className = "warnings";
    for (const warning of item.warnings) {
      const line = document.createElement("li");
      line.textContent = warning;
      warnings.append(line);
    }
    element.append(warnings);
  }
}

function updateActions() {
  downloadAll.hidden = items.filter((item) => item.status === "done").length < 2;
}

function release(item) {
  if (item.url) {
    URL.revokeObjectURL(item.url);
  }
  item.url = item.bytes = item.outputName = item.error = item.warnings = undefined;
}

function say(text) {
  message.textContent = text;
  message.hidden = !text;
}

function size(bytes) {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  const units = ["KB", "MB", "GB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`;
}

function save(blob, name) {
  const link = document.createElement("a");
  link.href = URL.createObjectURL(blob);
  link.download = name;
  link.click();
  setTimeout(() => URL.revokeObjectURL(link.href), 60_000);
}

// Two files can convert to the same name; number the later ones.
function uniqueNames(names) {
  const seen = new Map();
  return names.map((name) => {
    const count = (seen.get(name) ?? 0) + 1;
    seen.set(name, count);
    if (count === 1) {
      return name;
    }
    const dot = name.lastIndexOf(".");
    return `${name.slice(0, dot)} (${count})${name.slice(dot)}`;
  });
}

picker.addEventListener("change", () => {
  addFiles(picker.files);
  picker.value = "";
});

// Files can be dropped anywhere on the page.
let dragDepth = 0;
window.addEventListener("dragenter", (event) => {
  event.preventDefault();
  dragDepth += 1;
  drop.classList.add("over");
});
window.addEventListener("dragleave", () => {
  dragDepth = Math.max(0, dragDepth - 1);
  drop.classList.toggle("over", dragDepth > 0);
});
window.addEventListener("dragover", (event) => event.preventDefault());
window.addEventListener("drop", (event) => {
  event.preventDefault();
  dragDepth = 0;
  drop.classList.remove("over");
  addFiles(event.dataTransfer.files);
});

for (const radio of document.querySelectorAll('input[name="format"]')) {
  radio.addEventListener("change", () => {
    for (const item of items) {
      if (NOTE_FILE.test(item.file.name)) {
        queue(item);
      }
    }
    say("");
    updateActions();
    next();
  });
}

downloadAll.addEventListener("click", () => {
  const done = items.filter((item) => item.status === "done");
  const names = uniqueNames(done.map((item) => item.outputName));
  downloadAll.disabled = true;
  say("");
  worker.postMessage({
    type: "zip",
    id: "zip",
    entries: done.map((item, index) => ({ name: names[index], bytes: item.bytes })),
  });
});

clear.addEventListener("click", () => {
  for (const item of items) {
    release(item);
  }
  items = [];
  list.replaceChildren();
  section.hidden = true;
  say("");
  updateActions();
});
