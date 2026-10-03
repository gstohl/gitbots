import { render } from "solid-js/web";
import App from "./App";
import { captureToken } from "./api/client";
import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/components.css";
import "./styles/pages.css";
import "./styles/diff.css";

// Take `#token=...` out of the URL before the router sees it.
captureToken();

const root = document.getElementById("root");
if (!root) throw new Error("#root missing from index.html");
render(() => <App />, root);
