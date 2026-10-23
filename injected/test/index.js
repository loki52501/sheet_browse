// Entry point so that `node --test injected/test/` (D-401) runs every suite: Node 22 treats a
// directory argument as a module and loads this file.
"use strict";

require("./jsdom.test.js");
require("./browser.test.js");
require("./media.test.js");
