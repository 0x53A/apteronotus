import init from "./pkg/apteronotus_app.js";

const loading = document.querySelector("#loading");
const meter = document.querySelector("#loading-meter");
const progress = document.querySelector("#loading-progress");
const status = document.querySelector("#loading-status");
const detail = document.querySelector("#loading-detail");

function formatMebibytes(bytes) {
  return (bytes / (1024 * 1024)).toFixed(1);
}

async function trackedWasmResponse() {
  const response = await fetch(
    new URL("./pkg/apteronotus_app_bg.wasm", import.meta.url),
  );
  if (!response.ok) {
    throw new Error(`download returned ${response.status}`);
  }

  const total = Number(response.headers.get("Content-Length")) || 0;
  if (!response.body) {
    status.textContent = "Starting";
    return response;
  }

  const reader = response.body.getReader();
  let received = 0;
  let shownPercent = -1;

  if (total > 0) {
    meter.classList.remove("loading__meter--indeterminate");
    meter.setAttribute("aria-valuemin", "0");
    meter.setAttribute("aria-valuemax", "100");
  } else {
    status.textContent = "Downloading";
  }

  const stream = new ReadableStream({
    async pull(controller) {
      const { done, value } = await reader.read();
      if (done) {
        status.textContent = "Starting";
        detail.textContent = "Preparing the synthesis engine";
        controller.close();
        return;
      }

      received += value.byteLength;
      if (total > 0) {
        const percent = Math.min(100, Math.round((received / total) * 100));
        if (percent !== shownPercent) {
          shownPercent = percent;
          progress.style.width = `${percent}%`;
          meter.setAttribute("aria-valuenow", String(percent));
          status.textContent =
            `Downloading ${formatMebibytes(received)} / ` +
            `${formatMebibytes(total)} MiB`;
        }
      }
      controller.enqueue(value);
    },
    cancel(reason) {
      return reader.cancel(reason);
    },
  });

  return new Response(stream, {
    headers: response.headers,
    status: response.status,
    statusText: response.statusText,
  });
}

try {
  const wasm = await trackedWasmResponse();
  await init({ module_or_path: wasm });
} catch (error) {
  loading.classList.add("loading--error");
  meter.classList.remove("loading__meter--indeterminate");
  status.textContent = "Load failed";
  detail.textContent = error instanceof Error ? error.message : String(error);
  throw error;
}
