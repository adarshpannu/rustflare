
SELECT NAME FROM EMP
WHERE DEPT IN (SELECT D FROM DEPT)
;