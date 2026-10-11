package main

import (
	"encoding/json"
	"fmt"
)

type User struct {
	ID     int64     `json:"id"`
	Name   string    `json:"name"`
	Email  string    `json:"email"`
	Active bool      `json:"active"`
	Scores []float64 `json:"scores"`
	Tags   []string  `json:"tags"`
}

func main() {
	users := make([]User, 0)
	for i := int64(0); i < 50000; i++ {
		users = append(users, User{i, fmt.Sprintf("user %d", i), fmt.Sprintf("user%d@example.com", i), i%3 == 0,
			[]float64{float64(i) * 0.5, 1.25, 99.0}, []string{"alpha", "beta \"quoted\""}})
	}
	bytes, ids := 0, int64(0)
	for k := 0; k < 4; k++ {
		text, err := json.Marshal(users)
		if err != nil {
			panic(err)
		}
		bytes += len(text)
		var back []User
		if err := json.Unmarshal(text, &back); err != nil {
			panic(err)
		}
		for _, u := range back {
			ids += u.ID
		}
	}
	fmt.Println(bytes, ids)
}
