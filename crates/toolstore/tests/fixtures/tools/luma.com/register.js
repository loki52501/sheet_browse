// @name register
// @kind planner
// @match /*
// @args name:string email:string
// @gate Submit
// @fingerprint form#f2 button"Register" input"Email"
// @expects navigated:/thanks|/confirm OR text"You're registered"
// @phase 2
export default function (page, args) {
  const name = page.find('input "Full name"');
  const email = page.find('input "Email"');
  const btn = page.find('button "Register" SUBMIT');
  return `t#${name}=${q(args.name)}; t#${email}=${q(args.email)}; c#${btn}; ?s`;
}
