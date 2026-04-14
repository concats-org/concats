import { BrowserRouter, Routes, Route } from "react-router";
import { GithubProvider } from "@/hooks/use-pull-data";
import { HomePage } from "@/pages/home-page";
import { PullPage } from "@/pages/pull-page";

export default function App() {
  return (
    <GithubProvider>
      <BrowserRouter>
        <Routes>
          <Route path="/" element={<HomePage />} />
          <Route
            path="/github/:owner/:repo/pull/:pullNumber"
            element={<PullPage />}
          />
        </Routes>
      </BrowserRouter>
    </GithubProvider>
  );
}
