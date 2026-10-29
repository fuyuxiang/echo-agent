/** Module declarations for static asset imports. */
declare module "*?raw" {
  const content: string;
  export default content;
}
declare module "*.woff2?inline" {
  const source: string;
  export default source;
}
declare module "*.png" {
  const src: string;
  export default src;
}
declare module "*.jpg" {
  const src: string;
  export default src;
}
declare module "*.jpeg" {
  const src: string;
  export default src;
}
declare module "*.svg" {
  const src: string;
  export default src;
}
declare module "*.gif" {
  const src: string;
  export default src;
}
declare module "*.webp" {
  const src: string;
  export default src;
}

declare module "*?worker" {
  const WorkerFactory: {
    new (): Worker;
  };
  export default WorkerFactory;
}
