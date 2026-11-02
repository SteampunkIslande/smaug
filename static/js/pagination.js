export function createPagination({ root = document.getElementById("pagination"), onPage }) {
  if (!root) return { update() {} };

  const pageInput = root.querySelector("#page-input");
  const pageInfo = root.querySelector("#page-info");
  const buttons = [...root.querySelectorAll("[data-page-action]")];
  let currentPage = 1;
  let totalPages = 1;

  const go = (page) => {
    const target = Math.max(1, Math.min(totalPages, Number(page) || 1));
    if (target !== currentPage) onPage(target);
  };

  for (const button of buttons) {
    button.addEventListener("click", () => {
      const action = button.dataset.pageAction;
      if (action === "first") go(1);
      if (action === "prev") go(currentPage - 1);
      if (action === "next") go(currentPage + 1);
      if (action === "last") go(totalPages);
    });
  }

  pageInput?.addEventListener("change", () => go(pageInput.value));
  pageInput?.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      go(pageInput.value);
    }
  });

  return {
    update(pagination) {
      currentPage = Number(pagination?.current_page) || 1;
      totalPages = Math.max(1, Number(pagination?.total_pages) || 1);
      const multiple = totalPages > 1;
      root.hidden = !multiple;
      root.setAttribute("aria-hidden", String(!multiple));
      if (pageInput) pageInput.value = String(currentPage);
      if (pageInfo) pageInfo.textContent = `/ ${totalPages}`;
      const previousDisabled = !multiple || currentPage <= 1;
      const nextDisabled = !multiple || currentPage >= totalPages;
      root.querySelector('[data-page-action="first"]').disabled = previousDisabled;
      root.querySelector('[data-page-action="prev"]').disabled = previousDisabled;
      root.querySelector('[data-page-action="next"]').disabled = nextDisabled;
      root.querySelector('[data-page-action="last"]').disabled = nextDisabled;
    },
  };
}
