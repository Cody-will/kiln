package main

import (
	"io"
	"net/http"
)

func main() {
	http.HandleFunc("/hello", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		io.WriteString(w, "hello, world!\n")
	})
	http.ListenAndServe("127.0.0.1:8080", nil)
}
