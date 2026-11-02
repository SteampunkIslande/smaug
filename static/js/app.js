const path = window.location.pathname;
let current = "";
if (path === "/smaug/home") current = "home";
if (path.startsWith("/smaug/show/") || path.startsWith("/smaug/search/") || path.startsWith("/smaug/editrun/")) current = "runs";
if (path.startsWith("/smaug/admin")) current = "admin";
if (path.startsWith("/smaug/passedit/")) current = "account";
if (path.startsWith("/smaug/documentation")) current = "docs";

for (const link of document.querySelectorAll("[data-nav]")) {
  const isActive = link.dataset.nav === current;
  link.classList.toggle("active", isActive);
  if (isActive) link.setAttribute("aria-current", "page");
}
