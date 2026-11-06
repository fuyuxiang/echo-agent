import type { ReactNode } from "react";

/** Shared title anatomy. Pages keep their own actions and scroll ownership. */
export function PageHeading({ title, description, level = 1, className = "", titleClassName = "", descriptionClassName = "", children }: {
  title: ReactNode;
  description?: ReactNode;
  level?: 1 | 2;
  className?: string;
  titleClassName?: string;
  descriptionClassName?: string;
  children?: ReactNode;
}) {
  const Heading = level === 1 ? "h1" : "h2";
  return <div className={`page-heading ${className}`}>
    <Heading className={`page-heading__title ${titleClassName}`}>{title}</Heading>
    {description && <p className={`page-heading__description ${descriptionClassName}`}>{description}</p>}
    {children}
  </div>;
}
