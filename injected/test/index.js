// Entry point so that `node --test injected/test/` (D-401) runs every suite: Node 22 treats a
// directory argument as a module and loads this file. accept-t19.test.js is the T-19 acceptance suite.
"use strict";

require("./jsdom.test.js");
require("./browser.test.js");
require("./media.test.js");
require("./accept-t19.test.js");
