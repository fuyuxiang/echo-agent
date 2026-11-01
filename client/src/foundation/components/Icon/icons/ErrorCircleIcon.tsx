import { forwardRef } from "react";
import { createIcon } from "../Icon";

const ErrorCircleIconRaw = forwardRef<SVGSVGElement>((props, ref) => (
  <svg
      ref={ref}
      viewBox="0 0 14.2 14.2"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      {...props}
    >
      <path fill="var(--echo-status-error)" fillRule="evenodd" d="M7.1 1.2Q9.1261 1.2 10.7207 2.4413L11.4578 1.4943Q9.5381 0 7.1 0Q4.1591 0 2.0796 2.0796Q0 4.1591 0 7.1Q0 10.0409 2.0796 12.1205Q4.1591 14.2 7.1 14.2Q10.0409 14.2 12.1205 12.1205Q14.2 10.0409 14.2 7.1Q14.2 4.8675 12.9272 3.0426L11.9429 3.7291Q13 5.2447 13 7.1Q13 9.5439 11.2719 11.2719Q9.5439 13 7.1 13Q4.6561 13 2.9281 11.2719Q1.2 9.5439 1.2 7.1Q1.2 4.6561 2.9281 2.9281Q4.6561 1.2 7.1 1.2ZM7.6985 8.6V3.1H6.4985V8.6H7.6985ZM6.4985 11.1V9.9H7.6985V11.1H6.4985Z" />
    </svg>
));
ErrorCircleIconRaw.displayName = "ErrorCircleIconRaw";

export const ErrorCircleIcon = createIcon(ErrorCircleIconRaw);
