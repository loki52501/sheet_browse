-- B-187, D-144: harness-mem schedule.add is Account with the hint `schedule`. A hint rule
-- that decides an ask makes it time out to skip (D-110 (4)), so an unanswered schedule.add
-- (an unattended harnessd firing, a call planted by a page) adds nothing. The user can
-- answer yes, or change this row.
insert into gate.rules (domain, class, mode, note) values
  ('*','Account:schedule','ask','schedules are mine: no answer, no schedule (B-187)')
on conflict (domain, class) do nothing;
