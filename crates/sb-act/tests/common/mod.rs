//! Shared I-5 wire-JSON index fixtures for the T-19 sb-act acceptance tests. Items 1-6
//! (accept_t19_classify.rs) and items 7-11 (accept_t19_round2.rs) use the same rows, and
//! item 11's lowering guard walks every fixture listed in [`ALL`].
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

/// Every fixture in the acceptance suite, for item 11's lowering guard.
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
];
