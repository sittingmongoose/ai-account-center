/** Probe a disposable canvas before starting Slint's supported WebGL renderer. */
export function webGlAvailable(createCanvas = () => document.createElement('canvas')) {
  try {
    const context = createCanvas().getContext('webgl2');
    if (!context) return false;
    // The probe will not be rendered; release its separate GPU context promptly.
    context.getExtension?.('WEBGL_lose_context')?.loseContext();
    return true;
  } catch {
    return false;
  }
}

export const WEBGL_REQUIRED_MESSAGE = 'This dashboard needs WebGL. Enable hardware acceleration in your browser, then reload.';
export function requireWebGL(createCanvas) {
  if (webGlAvailable(createCanvas)) return;
  const error = new Error(WEBGL_REQUIRED_MESSAGE);
  error.code = 'webgl_required';
  throw error;
}

// winit 0.30.13's web EventLoop::run throws this exact signal after registering
// its browser callbacks. Other errors (including renderer failures) must surface.
// https://github.com/rust-windowing/winit/blob/v0.30.13/src/platform_impl/web/event_loop/mod.rs
const WINIT_WEB_LOOP_HANDOFF = "Using exceptions for control flow, don't mind me. This isn't actually an error!";
export function startSlintDashboard(start) {
  try {
    start();
  } catch (error) {
    const message = typeof error === 'string' ? error : error?.message;
    if (message !== WINIT_WEB_LOOP_HANDOFF) throw error;
  }
}
