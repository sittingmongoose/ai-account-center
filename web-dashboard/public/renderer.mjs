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

// Provider marks in dark mode (MARKS-HIDPI). Slint 1.18.1 on the web hands every SVG image to the
// browser as an <img> with an image/svg+xml blob URL, and femtovg 0.27 uploads that element with
// texSubImage2D. Slint flags those textures as premultiplied (i-slint-renderer-femtovg 1.18.1
// images.rs, `new_from_image`: "HTMLImageElement converts to a texture with pre-multiplied alpha"),
// but WebGL delivers straight alpha unless UNPACK_PREMULTIPLY_ALPHA_WEBGL is set, and femtovg never
// sets it. The shader then skips the alpha multiply, so every partly covered edge pixel of a
// full-colour mark is drawn at full colour: hard stair-stepped edges, a fattened silhouette and stray
// saturated pixels (red on Claude's tips). On a light surface the over-bright edge clips into the
// paper and hides; on a dark one it shows. Uploading exactly those SVG images premultiplied makes
// the data match Slint's flag. Other uploads (PNG images, which Slint flags straight and the shader
// premultiplies, glyph atlases, pixel buffers) keep the browser default.
const UNPACK_PREMULTIPLY_ALPHA_WEBGL = 0x9241;
const SVG_UPLOADS_PATCHED = Symbol.for('aac.premultiplySvgTextureUploads');
export function premultiplySvgTextureUploads(scope = globalThis) {
  const { URL: Url, HTMLImageElement: ImageElement } = scope;
  const contexts = [scope.WebGL2RenderingContext, scope.WebGLRenderingContext].filter(c => typeof c === 'function');
  if (typeof Url?.createObjectURL !== 'function' || typeof ImageElement !== 'function' || !contexts.length) return false;
  if (Url[SVG_UPLOADS_PATCHED]) return true;
  const svgUrls = new Set();
  const create = Url.createObjectURL, revoke = Url.revokeObjectURL;
  Url.createObjectURL = function createObjectURL(object) {
    const url = create.call(this, object);
    if (object?.type === 'image/svg+xml') svgUrls.add(url);
    return url;
  };
  if (typeof revoke === 'function') {
    Url.revokeObjectURL = function revokeObjectURL(url) {
      svgUrls.delete(url);
      return revoke.call(this, url);
    };
  }
  const isSvgImage = source => source instanceof ImageElement && svgUrls.has(source.src);
  for (const Context of contexts) {
    for (const name of ['texImage2D', 'texSubImage2D']) {
      const upload = Context.prototype[name];
      if (typeof upload !== 'function') continue;
      Context.prototype[name] = function (...args) {
        if (!isSvgImage(args[args.length - 1])) return upload.apply(this, args);
        const flag = this.UNPACK_PREMULTIPLY_ALPHA_WEBGL ?? UNPACK_PREMULTIPLY_ALPHA_WEBGL;
        const previous = this.getParameter(flag);
        this.pixelStorei(flag, true);
        try {
          return upload.apply(this, args);
        } finally {
          this.pixelStorei(flag, previous);
        }
      };
    }
  }
  Url[SVG_UPLOADS_PATCHED] = true;
  return true;
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
