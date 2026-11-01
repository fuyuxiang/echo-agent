import { forwardRef } from "react";
import { createIcon } from "../Icon";

const RunningCheckIconRaw = forwardRef<SVGSVGElement>((props, ref) => (
  <svg
      ref={ref}
      viewBox="0 0 14 14"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      {...props}
    >
      <path fill="var(--echo-status-success)" transform="matrix(1 0 0 1 0.406242 0.406086)" d="M7.5968 1.5299Q8.7661 1.7614 9.6971 2.4659L10.4036 1.6846Q9.2548 0.7878 7.8008 0.4999Q5.2765 0 3.1381 1.4315Q0.9997 2.863 0.4999 5.3873Q0 7.9116 1.4315 10.05Q2.863 12.1883 5.3873 12.6882Q7.9116 13.188 10.05 11.7566Q12.1883 10.3251 12.6882 7.8008Q12.9535 6.4612 12.6384 5.1519L11.7283 6.0281Q11.8148 6.8058 11.6582 7.5968Q11.2428 9.6945 9.4659 10.884Q7.6889 12.0736 5.5912 11.6582Q3.4936 11.2428 2.304 9.4659Q1.1145 7.6889 1.5299 5.5912Q1.9452 3.4936 3.7222 2.304Q5.4992 1.1145 7.5968 1.5299ZM4.7305 6.136Q4.8069 6.7977 5.0924 7.3746Q5.5307 8.2604 6.2777 7.9678Q7.3587 7.5443 9.4932 5.1938Q10.5876 3.9886 11.4442 2.8782L12.2756 3.5196Q11.3932 4.6633 10.2705 5.8997Q7.9707 8.4323 6.6607 8.9454Q5.766 9.296 5.0436 8.8725Q4.5061 8.5574 4.1513 7.8403Q3.7848 7.0996 3.6874 6.2565L4.7305 6.136Z" fillRule="evenodd" />
    </svg>
));
RunningCheckIconRaw.displayName = "RunningCheckIconRaw";

export const RunningCheckIcon = createIcon(RunningCheckIconRaw);
