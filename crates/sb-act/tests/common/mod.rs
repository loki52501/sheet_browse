//! Shared I-5 wire-JSON index fixtures for the T-19 sb-act acceptance tests. Items 1-6
//! (accept_t19_classify.rs), items 7-11 (accept_t19_round2.rs) and items 12-18
//! (accept_t19_round3.rs), items 19-23 (accept_t19_round4.rs) and items 24-28
//! (accept_t19_round5.rs) use the same rows, and the lowering guards (items 11, 18, 22,
//! 27 and 28) walk every fixture listed in [`ALL`].
#![allow(dead_code)]

/// bench/pages/register.html as the live walk would ship it (B-5): nav links, #signup (f1)
/// with email, new-password, full name, plan select, agree checkbox, the reCAPTCHA
/// container/iframe as one CHALLENGE + OPAQUE row (D-55) and the Register submit; the
/// newsletter form (f2); the cookie banner (OVERLAY). The hidden promo is skipped.
pub const REGISTER: &str = r#"[
  {"id":1,"frame":0,"role":"link","name":"Home","landmark":"nav"},
  {"id":2,"frame":0,"role":"link","name":"Pricing","landmark":"nav"},
  {"id":3,"frame":0,"role":"link","name":"Sign in","landmark":"nav"},
  {"id":4,"frame":0,"role":"textbox","name":"Email","heading":"Create your account",
   "state":{"value":{"kind":"empty"},"input_type":"email","autocomplete":"email"},
   "form":"f1","landmark":"main","box":{"x":16,"y":180,"w":608,"h":40}},
  {"id":5,"frame":0,"role":"textbox","name":"Password","heading":"Create your account",
   "state":{"value":{"kind":"empty"},"input_type":"password","autocomplete":"new-password"},
   "form":"f1","landmark":"main"},
  {"id":6,"frame":0,"role":"textbox","name":"Full name","heading":"Create your account",
   "state":{"value":{"kind":"empty"},"input_type":"text"},"form":"f1","landmark":"main"},
  {"id":7,"frame":0,"role":"combobox","name":"Plan","heading":"Create your account",
   "state":{"value":{"kind":"text","text":"Basic"},"options":["Basic","Plus","Pro"]},
   "form":"f1","landmark":"main"},
  {"id":8,"frame":0,"role":"checkbox","name":"I agree to the Terms of Service",
   "heading":"Create your account","state":{"checked":false,"input_type":"checkbox"},
   "form":"f1","landmark":"main"},
  {"id":9,"frame":0,"role":"iframe","name":"reCAPTCHA","heading":"Create your account",
   "flags":{"challenge":true,"opaque":true},"form":"f1","landmark":"main","child_frame":1},
  {"id":10,"frame":0,"role":"button","name":"Register","heading":"Create your account",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f1","landmark":"main"},
  {"id":11,"frame":0,"role":"textbox","name":"Email","heading":"Stay in the loop",
   "state":{"input_type":"email"},"form":"f2","landmark":"footer"},
  {"id":12,"frame":0,"role":"button","name":"Subscribe","heading":"Stay in the loop",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f2","landmark":"footer"},
  {"id":13,"frame":0,"role":"button","name":"Accept all","flags":{"overlay":true},"landmark":"region"},
  {"id":14,"frame":0,"role":"button","name":"Reject","flags":{"overlay":true},"landmark":"region"}
]"#;

/// A login page whose heading mentions create account (D-433: still a login, Submit).
pub const LOGIN: &str = r#"[
  {"id":20,"frame":0,"role":"textbox","name":"Email","heading":"Sign in or create an account",
   "state":{"input_type":"email","autocomplete":"username"},"form":"f4"},
  {"id":21,"frame":0,"role":"textbox","name":"Password","heading":"Sign in or create an account",
   "state":{"value":{"kind":"filled"},"input_type":"password","autocomplete":"current-password"},"form":"f4"},
  {"id":22,"frame":0,"role":"button","name":"Sign in","heading":"Sign in or create an account",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f4"}
]"#;

/// Saved-card checkout (D-58): no card field, only a saved-card radio and "Place order".
pub const CHECKOUT: &str = r#"[
  {"id":30,"frame":0,"role":"radio","name":"Visa ending 4242","heading":"Payment method",
   "state":{"checked":true,"input_type":"radio"},"form":"f5"},
  {"id":31,"frame":0,"role":"button","name":"Place order","heading":"Review your order",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f5"},
  {"id":32,"frame":0,"role":"link","name":"Return to cart"}
]"#;

/// Unsubscribe form, a cancel-something button, a bare Cancel and a bare Close.
pub const MANAGE: &str = r#"[
  {"id":40,"frame":0,"role":"textbox","name":"Email","heading":"Email preferences",
   "state":{"input_type":"email"},"form":"f6"},
  {"id":41,"frame":0,"role":"button","name":"Unsubscribe","heading":"Email preferences",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f6"},
  {"id":42,"frame":0,"role":"button","name":"Cancel","heading":"Email preferences","form":"f6"},
  {"id":43,"frame":0,"role":"button","name":"Close","landmark":"dialog"},
  {"id":44,"frame":0,"role":"button","name":"Cancel subscription","heading":"Your plan"},
  {"id":45,"frame":0,"role":"link","name":"Unsubscribe from all mail","landmark":"footer"}
]"#;

/// Job application with a CV upload, an e-sign form, and a frame-scoped submit.
pub const HINTS: &str = r#"[
  {"id":50,"frame":0,"role":"textbox","name":"Full name","heading":"Apply for Senior Engineer",
   "state":{"input_type":"text"},"form":"f7"},
  {"id":51,"frame":0,"role":"textbox","name":"Resume","heading":"Apply for Senior Engineer",
   "state":{"input_type":"file"},"form":"f7"},
  {"id":52,"frame":0,"role":"button","name":"Submit","heading":"Apply for Senior Engineer",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f7"},
  {"id":60,"frame":0,"role":"textbox","name":"Type your full name","heading":"Lease agreement",
   "state":{"input_type":"text"},"form":"f8"},
  {"id":61,"frame":0,"role":"button","name":"Sign document","heading":"Lease agreement",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f8"},
  {"id":15,"frame":2,"role":"textbox","name":"Comment","state":{"input_type":"text"},"form":"f1"},
  {"id":16,"frame":2,"role":"button","name":"Post","state":{"submitish":true},"flags":{"submit":true},"form":"f1"}
]"#;

/// D-403 (1) key steps: a mail toolbar Delete button, a SUBMIT "Place order" in a
/// saved-card checkout form, a nav link, a formless search box, a plain button, a mail
/// list row and its selection checkbox.
pub const KEYS: &str = r#"[
  {"id":100,"frame":0,"role":"button","name":"Delete","heading":"Inbox","landmark":"main"},
  {"id":101,"frame":0,"role":"button","name":"Place order","heading":"Review your order",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f20","landmark":"main"},
  {"id":102,"frame":0,"role":"radio","name":"Visa ending 4242","heading":"Review your order",
   "state":{"checked":true,"input_type":"radio"},"form":"f20","landmark":"main"},
  {"id":103,"frame":0,"role":"link","name":"Pricing","landmark":"nav"},
  {"id":104,"frame":0,"role":"textbox","name":"Search mail",
   "state":{"input_type":"text"},"landmark":"header"},
  {"id":105,"frame":0,"role":"button","name":"Archive","heading":"Inbox","landmark":"main"},
  {"id":106,"frame":0,"role":"row","name":"Alice Smith | Lunch on Friday | 10:02",
   "heading":"Inbox","landmark":"main"},
  {"id":107,"frame":0,"role":"checkbox","name":"Select conversation","heading":"Inbox",
   "state":{"checked":false,"input_type":"checkbox"},"landmark":"main"}
]"#;

/// D-403 (2) implicit submission: a card-only form with no submit row (f21), and a
/// formless chat composer with no heading.
pub const IMPLICIT: &str = r#"[
  {"id":110,"frame":0,"role":"textbox","name":"Card number","heading":"Card details",
   "state":{"input_type":"text","autocomplete":"cc-number"},"form":"f21"},
  {"id":111,"frame":0,"role":"textbox","name":"Expiry","heading":"Card details",
   "state":{"input_type":"text","autocomplete":"cc-exp"},"form":"f21"},
  {"id":112,"frame":0,"role":"textbox","name":"CVC","heading":"Card details",
   "state":{"input_type":"text","autocomplete":"cc-csc"},"form":"f21"},
  {"id":120,"frame":0,"role":"textbox","name":"Message","state":{"input_type":"text"}}
]"#;

/// D-403 (3)(4)(5) evidence. f22: "Change your email" heading over one current-password;
/// f23: the change is only in the form's name; f24: "Delete account?" heading with a
/// "Confirm" SUBMIT and a bare Cancel; f25..f28: card fields named CVC, Card expiry, MM / YY
/// and Security code with no autocomplete, under a neutral heading; f29: "Buyer info".
pub const EVIDENCE: &str = r#"[
  {"id":130,"frame":0,"role":"textbox","name":"New email","heading":"Change your email",
   "state":{"input_type":"email","autocomplete":"email"},"form":"f22"},
  {"id":131,"frame":0,"role":"textbox","name":"Password","heading":"Change your email",
   "state":{"value":{"kind":"filled"},"input_type":"password","autocomplete":"current-password"},
   "form":"f22"},
  {"id":132,"frame":0,"role":"button","name":"Save","heading":"Change your email",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f22"},
  {"id":135,"frame":0,"role":"form","name":"Change email address","heading":"Settings","form":"f23"},
  {"id":136,"frame":0,"role":"textbox","name":"Email","heading":"Settings",
   "state":{"input_type":"email"},"form":"f23"},
  {"id":137,"frame":0,"role":"textbox","name":"Current password","heading":"Settings",
   "state":{"input_type":"password","autocomplete":"current-password"},"form":"f23"},
  {"id":138,"frame":0,"role":"button","name":"Update","heading":"Settings",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f23"},
  {"id":140,"frame":0,"role":"textbox","name":"Type your password","heading":"Delete account?",
   "state":{"input_type":"password","autocomplete":"current-password"},"form":"f24"},
  {"id":141,"frame":0,"role":"button","name":"Confirm","heading":"Delete account?",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f24"},
  {"id":142,"frame":0,"role":"button","name":"Cancel","heading":"Delete account?","form":"f24"},
  {"id":150,"frame":0,"role":"textbox","name":"CVC","heading":"Your details",
   "state":{"input_type":"text"},"form":"f25"},
  {"id":151,"frame":0,"role":"button","name":"Continue","heading":"Your details",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f25"},
  {"id":152,"frame":0,"role":"textbox","name":"Card expiry","heading":"Your details",
   "state":{"input_type":"text"},"form":"f26"},
  {"id":153,"frame":0,"role":"button","name":"Continue","heading":"Your details",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f26"},
  {"id":154,"frame":0,"role":"textbox","name":"MM / YY","heading":"Your details",
   "state":{"input_type":"text"},"form":"f27"},
  {"id":155,"frame":0,"role":"button","name":"Next","heading":"Your details",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f27"},
  {"id":156,"frame":0,"role":"textbox","name":"Security code","heading":"Your details",
   "state":{"input_type":"text"},"form":"f28"},
  {"id":157,"frame":0,"role":"button","name":"Continue","heading":"Your details",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f28"},
  {"id":160,"frame":0,"role":"textbox","name":"Full name","heading":"Buyer info",
   "state":{"input_type":"text"},"form":"f29"},
  {"id":161,"frame":0,"role":"textbox","name":"Email","heading":"Buyer info",
   "state":{"input_type":"email","autocomplete":"email"},"form":"f29"},
  {"id":162,"frame":0,"role":"button","name":"Continue","heading":"Buyer info",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f29"}
]"#;

/// D-404. f30: a "Two-step verification" code prompt (Next and Verify); f31: "Enable
/// two-factor authentication" over a password confirm; f32: the set-up phrase only on the
/// button; a "Delete account?" dialog without a form (Confirm, Cancel, both non-submit) and
/// a "Save changes?" dialog next to it whose Confirm must not borrow Delete.
pub const D404: &str = r#"[
  {"id":170,"frame":0,"role":"textbox","name":"Enter the code","heading":"Two-step verification",
   "state":{"input_type":"text","autocomplete":"one-time-code"},"form":"f30"},
  {"id":171,"frame":0,"role":"button","name":"Next","heading":"Two-step verification",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f30"},
  {"id":172,"frame":0,"role":"button","name":"Verify","heading":"Two-step verification",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f30"},
  {"id":175,"frame":0,"role":"textbox","name":"Confirm your password",
   "heading":"Enable two-factor authentication",
   "state":{"input_type":"password","autocomplete":"current-password"},"form":"f31"},
  {"id":176,"frame":0,"role":"button","name":"Continue","heading":"Enable two-factor authentication",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f31"},
  {"id":177,"frame":0,"role":"button","name":"Enable two-factor authentication","heading":"Security",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f32"},
  {"id":180,"frame":0,"role":"button","name":"Confirm","heading":"Delete account?","landmark":"dialog"},
  {"id":181,"frame":0,"role":"button","name":"Cancel","heading":"Delete account?","landmark":"dialog"},
  {"id":182,"frame":0,"role":"button","name":"Confirm","heading":"Save changes?","landmark":"dialog"}
]"#;

/// D-406 (1)(2)(3). f40: a card form (cc-number, cc-name) with a "Save card" switch, a
/// "Save card for next time" checkbox, a Terms link, a saved-card radio and a neutral
/// "Continue" SUBMIT; f41: a login with a non-submit "Show password" button; #215 and #216:
/// formless file inputs (role textbox and role button); f43: "CVC" with autocomplete
/// one-time-code; f44: "Security code" with autocomplete one-time-code.
pub const D406: &str = r#"[
  {"id":200,"frame":0,"role":"textbox","name":"Card number","heading":"Your details",
   "state":{"input_type":"text","autocomplete":"cc-number"},"form":"f40"},
  {"id":201,"frame":0,"role":"textbox","name":"Name on card","heading":"Your details",
   "state":{"input_type":"text","autocomplete":"cc-name"},"form":"f40"},
  {"id":202,"frame":0,"role":"switch","name":"Save card","heading":"Your details",
   "state":{"checked":false,"input_type":"checkbox"},"form":"f40"},
  {"id":203,"frame":0,"role":"checkbox","name":"Save card for next time","heading":"Your details",
   "state":{"checked":false,"input_type":"checkbox"},"form":"f40"},
  {"id":204,"frame":0,"role":"link","name":"Terms of sale","heading":"Your details","form":"f40"},
  {"id":205,"frame":0,"role":"radio","name":"Visa ending 4242","heading":"Your details",
   "state":{"checked":true,"input_type":"radio"},"form":"f40"},
  {"id":206,"frame":0,"role":"button","name":"Continue","heading":"Your details",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f40"},
  {"id":210,"frame":0,"role":"textbox","name":"Email","heading":"Sign in",
   "state":{"input_type":"email","autocomplete":"username"},"form":"f41"},
  {"id":211,"frame":0,"role":"textbox","name":"Password","heading":"Sign in",
   "state":{"input_type":"password","autocomplete":"current-password"},"form":"f41"},
  {"id":212,"frame":0,"role":"button","name":"Show password","heading":"Sign in","form":"f41"},
  {"id":213,"frame":0,"role":"button","name":"Sign in","heading":"Sign in",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f41"},
  {"id":215,"frame":0,"role":"textbox","name":"Upload ID","heading":"Documents",
   "state":{"input_type":"file"}},
  {"id":216,"frame":0,"role":"button","name":"Choose file","heading":"Documents",
   "state":{"input_type":"file"}},
  {"id":220,"frame":0,"role":"textbox","name":"CVC","heading":"Your details",
   "state":{"input_type":"text","autocomplete":"one-time-code"},"form":"f43"},
  {"id":221,"frame":0,"role":"button","name":"Continue","heading":"Your details",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f43"},
  {"id":222,"frame":0,"role":"textbox","name":"Security code","heading":"Your details",
   "state":{"input_type":"text","autocomplete":"one-time-code"},"form":"f44"},
  {"id":223,"frame":0,"role":"button","name":"Continue","heading":"Your details",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f44"}
]"#;

/// D-406 (4): #14 a "Sign in" SUBMIT with its email field #7, #42 a reCAPTCHA CHALLENGE.
pub const PLANNER: &str = r#"[
  {"id":7,"frame":0,"role":"textbox","name":"Email","heading":"Sign in",
   "state":{"input_type":"email","autocomplete":"username"},"form":"f1"},
  {"id":14,"frame":0,"role":"button","name":"Sign in","heading":"Sign in",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f1"},
  {"id":42,"frame":0,"role":"iframe","name":"reCAPTCHA","heading":"Sign in",
   "flags":{"challenge":true,"opaque":true},"child_frame":1}
]"#;

/// D-406 (6)(7) and D-407. #230..#232: affirmative buttons with no form and no dialog
/// landmark; f50..f57: one neutral "Step 3" form per new Pay word on its SUBMIT;
/// #248..#251: Upgrade/Renew/Order history outside a form; f60: "Renewal date"; f61: "Book
/// now"; f62..f65: passkeys; #270..#273: clear/empty buttons; #280, #281: "Cancel and/or
/// ..." in a dialog.
pub const D407: &str = r#"[
  {"id":230,"frame":0,"role":"button","name":"Confirm","heading":"Delete account?"},
  {"id":231,"frame":0,"role":"button","name":"Cancel","heading":"Delete account?"},
  {"id":232,"frame":0,"role":"button","name":"Confirm","heading":"Save changes?"},
  {"id":240,"frame":0,"role":"button","name":"Submit order","heading":"Step 3",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f50"},
  {"id":241,"frame":0,"role":"button","name":"Upgrade","heading":"Step 3",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f51"},
  {"id":242,"frame":0,"role":"button","name":"Complete order","heading":"Step 3",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f52"},
  {"id":243,"frame":0,"role":"button","name":"Order now","heading":"Step 3",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f53"},
  {"id":244,"frame":0,"role":"button","name":"Top up","heading":"Step 3",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f54"},
  {"id":245,"frame":0,"role":"button","name":"Send money","heading":"Step 3",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f55"},
  {"id":246,"frame":0,"role":"button","name":"Transfer funds","heading":"Step 3",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f56"},
  {"id":247,"frame":0,"role":"button","name":"Renew","heading":"Step 3",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f57"},
  {"id":248,"frame":0,"role":"button","name":"Upgrade","heading":"Your plan"},
  {"id":249,"frame":0,"role":"link","name":"Renew","heading":"Your plan"},
  {"id":250,"frame":0,"role":"link","name":"Upgrade","landmark":"nav"},
  {"id":251,"frame":0,"role":"link","name":"Order history","landmark":"nav"},
  {"id":252,"frame":0,"role":"textbox","name":"Renewal date","heading":"Renewal date",
   "state":{"input_type":"date"},"form":"f60"},
  {"id":253,"frame":0,"role":"button","name":"Save","heading":"Renewal date",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f60"},
  {"id":254,"frame":0,"role":"button","name":"Book now","heading":"Step 3",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f61"},
  {"id":260,"frame":0,"role":"button","name":"Create a passkey","heading":"Security",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f62"},
  {"id":261,"frame":0,"role":"button","name":"Continue","heading":"Create a passkey",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f63"},
  {"id":262,"frame":0,"role":"button","name":"Continue","heading":"Sign in with a passkey",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f64"},
  {"id":263,"frame":0,"role":"button","name":"Sign in with a passkey","heading":"Welcome back",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f65"},
  {"id":270,"frame":0,"role":"button","name":"Clear history","heading":"History"},
  {"id":271,"frame":0,"role":"button","name":"Clear browsing data","heading":"Privacy"},
  {"id":272,"frame":0,"role":"button","name":"Clear all","heading":"Notifications"},
  {"id":273,"frame":0,"role":"button","name":"Empty trash","heading":"Trash"},
  {"id":280,"frame":0,"role":"button","name":"Cancel and close","landmark":"dialog"},
  {"id":281,"frame":0,"role":"button","name":"Cancel or go back","landmark":"dialog"}
]"#;

/// D-408 (1)(3). f70: a card form ("Payment") with non-submit buttons: Pay words
/// (#302 "Complete purchase", #303 "Confirm", menuitem #304 "Place bid"), no Pay word
/// (#305 "Show details", #306 "Back to top"), a link #307 and a checkbox #308; f71: a
/// "Checkout" form with a non-submit "Submit" and "Edit address"; f72: a neutral form
/// with Pay-word buttons and a bare "Confirm"; f73: signup with a non-submit "Create
/// account"; f74: a login with one; f75: a "Change password" button; #350..#353: formless
/// buttons and a link sharing the "Pay with card" heading of f76; #355/#356: a formless
/// "Add" sharing "Passkeys" with f77; #360..#362: formless buttons with no shared heading
/// (#362 has the "Pay with card" heading, but in frame 1, where no form row is); #370..#379:
/// leave group and wipe device, with negatives.
pub const D408: &str = r#"[
  {"id":300,"frame":0,"role":"textbox","name":"Card number","heading":"Payment",
   "state":{"input_type":"text","autocomplete":"cc-number"},"form":"f70"},
  {"id":301,"frame":0,"role":"textbox","name":"Promo code","heading":"Payment",
   "state":{"input_type":"text"},"form":"f70"},
  {"id":302,"frame":0,"role":"button","name":"Complete purchase","heading":"Payment","form":"f70"},
  {"id":303,"frame":0,"role":"button","name":"Confirm","heading":"Payment","form":"f70"},
  {"id":304,"frame":0,"role":"menuitem","name":"Place bid","heading":"Payment","form":"f70"},
  {"id":305,"frame":0,"role":"button","name":"Show details","heading":"Payment","form":"f70"},
  {"id":306,"frame":0,"role":"button","name":"Back to top","heading":"Payment","form":"f70"},
  {"id":307,"frame":0,"role":"link","name":"Complete purchase","heading":"Payment","form":"f70"},
  {"id":308,"frame":0,"role":"checkbox","name":"Subscribe to offers","heading":"Payment",
   "state":{"checked":false,"input_type":"checkbox"},"form":"f70"},
  {"id":310,"frame":0,"role":"textbox","name":"Full name","heading":"Checkout",
   "state":{"input_type":"text"},"form":"f71"},
  {"id":311,"frame":0,"role":"button","name":"Submit","heading":"Checkout","form":"f71"},
  {"id":312,"frame":0,"role":"button","name":"Edit address","heading":"Checkout","form":"f71"},
  {"id":320,"frame":0,"role":"textbox","name":"Amount","heading":"Step 2",
   "state":{"input_type":"text"},"form":"f72"},
  {"id":321,"frame":0,"role":"button","name":"Pay now","heading":"Step 2","form":"f72"},
  {"id":322,"frame":0,"role":"button","name":"Buy now","heading":"Step 2","form":"f72"},
  {"id":323,"frame":0,"role":"button","name":"Donate","heading":"Step 2","form":"f72"},
  {"id":324,"frame":0,"role":"button","name":"Top up","heading":"Step 2","form":"f72"},
  {"id":325,"frame":0,"role":"button","name":"Renew","heading":"Step 2","form":"f72"},
  {"id":326,"frame":0,"role":"button","name":"Order now","heading":"Step 2","form":"f72"},
  {"id":327,"frame":0,"role":"button","name":"Confirm","heading":"Step 2","form":"f72"},
  {"id":330,"frame":0,"role":"textbox","name":"Email","heading":"Sign up",
   "state":{"input_type":"email"},"form":"f73"},
  {"id":331,"frame":0,"role":"button","name":"Create account","heading":"Sign up","form":"f73"},
  {"id":332,"frame":0,"role":"button","name":"Show password","heading":"Sign up","form":"f73"},
  {"id":335,"frame":0,"role":"textbox","name":"Email","heading":"Sign in",
   "state":{"input_type":"email","autocomplete":"username"},"form":"f74"},
  {"id":336,"frame":0,"role":"textbox","name":"Password","heading":"Sign in",
   "state":{"input_type":"password","autocomplete":"current-password"},"form":"f74"},
  {"id":337,"frame":0,"role":"button","name":"Create account","heading":"Sign in","form":"f74"},
  {"id":338,"frame":0,"role":"button","name":"Sign in","heading":"Sign in",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f74"},
  {"id":340,"frame":0,"role":"textbox","name":"Email","heading":"Profile",
   "state":{"input_type":"email"},"form":"f75"},
  {"id":341,"frame":0,"role":"button","name":"Change password","heading":"Profile","form":"f75"},
  {"id":350,"frame":0,"role":"textbox","name":"Card number","heading":"Pay with card",
   "state":{"input_type":"text","autocomplete":"cc-number"},"form":"f76"},
  {"id":351,"frame":0,"role":"button","name":"Complete","heading":"Pay with card"},
  {"id":352,"frame":0,"role":"button","name":"Show details","heading":"Pay with card"},
  {"id":353,"frame":0,"role":"link","name":"Complete","heading":"Pay with card"},
  {"id":355,"frame":0,"role":"textbox","name":"Name","heading":"Passkeys",
   "state":{"input_type":"text"},"form":"f77"},
  {"id":356,"frame":0,"role":"button","name":"Add","heading":"Passkeys"},
  {"id":360,"frame":0,"role":"button","name":"Create account"},
  {"id":361,"frame":0,"role":"button","name":"Create account","heading":"Welcome"},
  {"id":362,"frame":1,"role":"button","name":"Complete","heading":"Pay with card"},
  {"id":370,"frame":0,"role":"button","name":"Leave group","heading":"Family"},
  {"id":371,"frame":0,"role":"button","name":"Leave this group","heading":"Family"},
  {"id":372,"frame":0,"role":"button","name":"Wipe device","heading":"Devices"},
  {"id":373,"frame":0,"role":"button","name":"Wipe this device","heading":"Devices"},
  {"id":374,"frame":0,"role":"button","name":"Leave group","heading":"Group settings",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f78"},
  {"id":375,"frame":0,"role":"button","name":"Confirm","heading":"Leave group?"},
  {"id":376,"frame":0,"role":"button","name":"Leave feedback","heading":"Family"},
  {"id":377,"frame":0,"role":"button","name":"Leave a comment","heading":"Family"},
  {"id":378,"frame":0,"role":"button","name":"Group settings","heading":"Family"},
  {"id":379,"frame":0,"role":"button","name":"Device settings","heading":"Devices"}
]"#;

/// D-409 (1)(2). #400..#404: a formless Inbox (mail row, Archive button, selection
/// checkbox, search box, nav link); f80: a card form with a "Save card" checkbox and a
/// neutral SUBMIT; f81: signup with a "Remember me" checkbox. #420..#429: formless buttons
/// and menuitems named with a pay_words phrase (#422 sits under "Checkout" while the card
/// form #f2.423 is in frame 2, Stripe-style); #430..#447: negatives (generic names, single
/// Pay words without a form, Pay links, Pay-named checkables, look-alike words).
pub const D409: &str = r#"[
  {"id":400,"frame":0,"role":"row","name":"Bob Jones | Invoice 1042 | 09:14","heading":"Inbox",
   "landmark":"main"},
  {"id":401,"frame":0,"role":"button","name":"Archive","heading":"Inbox","landmark":"main"},
  {"id":402,"frame":0,"role":"checkbox","name":"Select conversation","heading":"Inbox",
   "state":{"checked":false,"input_type":"checkbox"},"landmark":"main"},
  {"id":403,"frame":0,"role":"textbox","name":"Search mail","state":{"input_type":"text"},
   "landmark":"header"},
  {"id":404,"frame":0,"role":"link","name":"Drafts","landmark":"nav"},
  {"id":405,"frame":0,"role":"textbox","name":"Card number","heading":"Payment",
   "state":{"input_type":"text","autocomplete":"cc-number"},"form":"f80"},
  {"id":406,"frame":0,"role":"checkbox","name":"Save card","heading":"Payment",
   "state":{"checked":false,"input_type":"checkbox"},"form":"f80"},
  {"id":407,"frame":0,"role":"button","name":"Continue","heading":"Payment",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f80"},
  {"id":408,"frame":0,"role":"textbox","name":"Email","heading":"Sign up",
   "state":{"input_type":"email","autocomplete":"email"},"form":"f81"},
  {"id":409,"frame":0,"role":"checkbox","name":"Remember me","heading":"Sign up",
   "state":{"checked":false,"input_type":"checkbox"},"form":"f81"},
  {"id":410,"frame":0,"role":"button","name":"Create account","heading":"Sign up",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f81"},
  {"id":420,"frame":0,"role":"button","name":"Pay now"},
  {"id":421,"frame":0,"role":"button","name":"Pay $20.00","heading":"Order summary"},
  {"id":422,"frame":0,"role":"button","name":"Complete purchase","heading":"Checkout"},
  {"id":423,"frame":2,"role":"textbox","name":"Card number","heading":"Checkout",
   "state":{"input_type":"text","autocomplete":"cc-number"},"form":"f1"},
  {"id":424,"frame":0,"role":"menuitem","name":"Buy now"},
  {"id":425,"frame":0,"role":"button","name":"Place your order","heading":"Order summary"},
  {"id":426,"frame":0,"role":"button","name":"Checkout","landmark":"header"},
  {"id":427,"frame":0,"role":"button","name":"Subscribe for $9.99/month","heading":"Plans"},
  {"id":428,"frame":0,"role":"menuitem","name":"Payment methods","heading":"Account"},
  {"id":429,"frame":0,"role":"button","name":"Donate","heading":"Support us"},
  {"id":430,"frame":0,"role":"button","name":"Back to top"},
  {"id":431,"frame":0,"role":"button","name":"Show details","heading":"Order summary"},
  {"id":432,"frame":0,"role":"button","name":"Confirm","heading":"Checkout"},
  {"id":433,"frame":0,"role":"link","name":"Pay","heading":"Order summary"},
  {"id":434,"frame":0,"role":"link","name":"Pay now"},
  {"id":435,"frame":0,"role":"link","name":"Checkout","landmark":"nav"},
  {"id":436,"frame":0,"role":"button","name":"Complete","heading":"Checkout"},
  {"id":437,"frame":0,"role":"radio","name":"Pay with PayPal","heading":"Order summary",
   "state":{"checked":false,"input_type":"radio"}},
  {"id":438,"frame":0,"role":"checkbox","name":"Pay later","heading":"Order summary",
   "state":{"checked":false,"input_type":"checkbox"}},
  {"id":439,"frame":0,"role":"tab","name":"Payment","heading":"Account"},
  {"id":440,"frame":0,"role":"button","name":"Repay later","heading":"Loans"},
  {"id":441,"frame":0,"role":"button","name":"Payroll settings","heading":"Account"},
  {"id":442,"frame":0,"role":"button","name":"Order history","heading":"Account"},
  {"id":443,"frame":0,"role":"button","name":"Display options","heading":"Account"},
  {"id":444,"frame":0,"role":"button","name":"Subscribe","heading":"Newsletter"},
  {"id":445,"frame":0,"role":"button","name":"Book now","heading":"Hotel"},
  {"id":446,"frame":0,"role":"button","name":"Delegate","heading":"Inbox"},
  {"id":447,"frame":0,"role":"button","name":"Top","heading":"Order summary"}
]"#;

/// Every fixture in the acceptance suite, for the lowering guards (items 11, 18, 22, 27
/// and 28). Adding a fixture means regenerating both raise-only baselines in
/// `bench/fixtures/` (see the round-5 section of `bench/results/accept-T-19-sb-act.md`).
pub const ALL: &[(&str, &str)] = &[
    ("REGISTER", REGISTER),
    ("LOGIN", LOGIN),
    ("CHECKOUT", CHECKOUT),
    ("MANAGE", MANAGE),
    ("HINTS", HINTS),
    ("KEYS", KEYS),
    ("IMPLICIT", IMPLICIT),
    ("EVIDENCE", EVIDENCE),
    ("D404", D404),
    ("D406", D406),
    ("PLANNER", PLANNER),
    ("D407", D407),
    ("D408", D408),
    ("D409", D409),
];

/// Item 22 (raise-only, hard rule 4): the key values every row is probed with, besides a
/// click, a type, a select and an upload. The pre-D-408 baseline in
/// `bench/fixtures/t19_pre_d408_classes.txt` was written with exactly this list, in this
/// order; changing it means regenerating the baseline with `crates/sb-act/src` at 3122050.
pub const RAISE_KEYS: &[&str] = &[
    "Enter",
    "Space",
    "Tab",
    "Delete",
    "a",
    "\r",
    "\n",
    "Enter+Shift",
    "Shift+Enter",
    " Enter",
    "Enter\t",
    "NumpadEnter",
    "Return",
    "Enterprise",
];

/// Item 22: the programs every row is probed with, as `(label, program)`. Key values go
/// through `quote` (D-52) so control characters survive the parser.
pub fn raise_programs(r: &sb_sheet::IndexRow) -> Vec<(String, String)> {
    let rf = if r.frame == 0 {
        format!("#{}", r.id)
    } else {
        format!("#f{}.{}", r.frame, r.id)
    };
    let mut out = vec![
        ("c".to_string(), format!("c{rf}")),
        ("t".to_string(), format!("t{rf}=x")),
        ("s".to_string(), format!("s{rf}=x")),
        ("u".to_string(), format!("u{rf}=files/a.pdf")),
    ];
    for (i, k) in RAISE_KEYS.iter().enumerate() {
        out.push((format!("k{i}"), format!("k{rf}={}", sb_act::quote(k))));
    }
    out
}

/// Item 28 (D-409): key values added to [`RAISE_KEYS`] for the raise-only guard
/// against the 0554c29 baseline `bench/fixtures/t19_pre_d409_classes.txt`. Changing this
/// list means regenerating that baseline with `crates/sb-act/src` at 0554c29.
pub const RAISE_KEYS_D409: &[&str] = &[
    "Enter+Delete",
    "Enter+Del",
    "Control+Enter+Delete",
    "Return+Shift+Delete",
    "Enter\u{7f}",
    "\u{7f}Enter",
    "Delete+Enter",
    "Del Enter",
    "\rDelete",
    "Shift+Delete",
    "Control+Delete",
    "Del",
    "\u{7f}",
    "Delete+a",
    "Delegate",
];

/// Item 28: [`raise_programs`] plus one `k` probe per [`RAISE_KEYS_D409`] value,
/// labelled `d0`, `d1`...
pub fn raise_programs_d409(r: &sb_sheet::IndexRow) -> Vec<(String, String)> {
    let rf = if r.frame == 0 {
        format!("#{}", r.id)
    } else {
        format!("#f{}.{}", r.frame, r.id)
    };
    let mut out = raise_programs(r);
    for (i, k) in RAISE_KEYS_D409.iter().enumerate() {
        out.push((format!("d{i}"), format!("k{rf}={}", sb_act::quote(k))));
    }
    out
}
