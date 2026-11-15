// Loader hook for generate.mjs: while the dashboard's own JS tests run, every import of view-model.mjs gets
// record-view-model.mjs instead, which notes each Home build (its inputs) before running the real one.
const WRAPPER = new URL('./record-view-model.mjs', import.meta.url).href;
export async function resolve(specifier, context, next) {
  const result = await next(specifier, context);
  if (result.url.endsWith('/web-dashboard/public/view-model.mjs') && context.parentURL !== WRAPPER)
    return { ...result, url: WRAPPER, shortCircuit: true };
  return result;
}
